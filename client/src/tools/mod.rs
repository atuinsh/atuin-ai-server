use std::io::BufRead;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::time::Duration;

use easy_cast::Conv;
use enum_dispatch::enum_dispatch;
use eyre::Result;
use serde::Deserialize;
use strum_macros::{EnumIter, EnumString, IntoStaticStr};

const DEFAULT_FILE_READ_LINES: u64 = 100;
const MAX_FILE_READ_LINES: u64 = 1000;
/// Page-size bounds for `atuin_history`; mirrored in the server's tool schema.
pub const DEFAULT_HISTORY_RESULTS: i64 = 10;
pub const MAX_HISTORY_RESULTS: i64 = 50;
pub mod descriptor;

use crate::ansi;
use crate::history::{self, History, HistoryDb, SearchMode};
use crate::permissions::rule::Rule;

/// `path` made absolute against the working directory, with `.` and `..` removed but symlinks
/// left alone: the spelling a user's rules are most likely written in.
fn absolute_path(path: &Path) -> PathBuf {
    let absolute =
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path));
    absolute.components().fold(PathBuf::new(), |mut out, component| {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
        out
    })
}

/// The file a tool's `path` names: absolute, with `.`, `..` and symlinks resolved, so a write goes
/// through a symlink rather than replacing it.
///
/// A path that doesn't exist yet is resolved a component at a time, in order, as the kernel would
/// resolve it once its missing directories exist: a `..` after a symlink steps out of its target,
/// not out of the directory holding the link.
fn resolve_tool_path(path: &Path) -> PathBuf {
    let absolute =
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path));
    if let Ok(real) = std::fs::canonicalize(&absolute) {
        return real;
    }
    absolute.components().fold(PathBuf::new(), |mut resolved, component| {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other);
                if let Ok(real) = std::fs::canonicalize(&resolved) {
                    resolved = real;
                }
            }
        }
        resolved
    })
}

/// The directory whose permission files apply to the file tool target `resolved`: the target
/// itself when it is a directory, as `read_file` lists one, else its parent.
fn permission_dir(resolved: PathBuf) -> Option<PathBuf> {
    if resolved.is_dir() {
        return Some(resolved);
    }
    resolved.parent().map(Path::to_path_buf)
}

/// Whether a rule's `scope` glob matches the file `path` names, spelled either as written or
/// resolved. A deny rule then catches an alias of the file it names, such as `a/../secret` or a
/// symlink, and a rule written through a symlinked directory (`/tmp` on macOS) still matches.
fn file_matches_scope(path: &Path, scope: &str) -> bool {
    [absolute_path(path), resolve_tool_path(path)]
        .iter()
        .any(|spelling| path_matches_scope(spelling, scope))
}

/// Check whether a file path matches a scope glob pattern.
///
/// Resolves relative paths against the current directory before matching so
/// that `./foo.md` and `/cwd/foo.md` match the same glob. Supports `*`, `**`,
/// `?`, and `[...]` via `glob_match`.
fn path_matches_scope(path: &Path, scope: &str) -> bool {
    let path = if path.is_relative() {
        std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    // Normalize to forward slashes so globs work on Windows too.
    let path_str = path.to_string_lossy().replace('\\', "/");

    // If the scope is also relative, try matching against both the absolute
    // path and just the filename/relative portion.
    if !scope.starts_with('/') {
        // Match against filename (e.g. "*.md" matches any .md file)
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && glob_match::glob_match(scope, name)
        {
            return true;
        }
        // Also try matching against the full absolute path in case the scope
        // is a relative multi-segment pattern like "crates/**/*.rs"
        if glob_match::glob_match(scope, &path_str) {
            return true;
        }
        // And match relative to cwd (so "src/*.rs" works from project root), spelled either way
        // like the path.
        let Ok(cwd) = std::env::current_dir() else {
            return false;
        };
        let resolved_cwd = std::fs::canonicalize(&cwd).ok();
        return [Some(cwd), resolved_cwd].into_iter().flatten().any(|cwd| {
            path.strip_prefix(&cwd).is_ok_and(|rel| {
                glob_match::glob_match(scope, &rel.to_string_lossy().replace('\\', "/"))
            })
        });
    }

    // Absolute scope — match against absolute path
    glob_match::glob_match(scope, &path_str)
}

/// Result of executing a client-side tool.
#[derive(Debug, Clone)]
pub enum ToolOutcome {
    /// Simple success with a text result (used by Read, AtuinHistory).
    Success(String),
    /// Error with a message.
    Error(String),
    /// Structured shell result with separated stdout, stderr, exit code, and duration.
    Structured {
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
        duration_ms: u64,
        interrupted: bool,
    },
}

impl ToolOutcome {
    /// Maximum size of the `stdout` and `stderr` buffers (each) in [`Self::Structured`].
    ///
    /// [`ansi::to_plain_text`] used to be capped at 16384 rows to prevent OOM errors, which
    /// provided an effective limit on the size of the stdout and stderr buffers we return. Now that
    /// [`ansi::to_plain_text`] is more efficient, it imposes no limit of its own; to ensure we
    /// don't return too much data, we apply the cap ourselves.
    ///
    /// 16384 times 120 (the width of the emulated terminal, from `PREVIEW_WIDTH`) is approximately
    /// 2,000,000 (2MB), so we use that as our limit here.
    ///
    /// Note that we keep the *end* of output that exceeds the limit. This matches what
    /// [`ansi::to_plain_text`] did previously -- it would process the full output but only keep the
    /// last 16384 rendered lines.
    const MAX_STRUCTURED_OUTPUT_SIZE: usize = 2_000_000;

    /// Format this outcome as a string for the tool result sent to the LLM.
    ///
    /// The optional `interrupt_reason` overrides the generic interrupted message
    /// with a specific one (user interrupt vs timeout).
    pub fn format_for_llm(
        &self,
        interrupt_reason: Option<&crate::fsm::tools::InterruptReason>,
    ) -> String {
        match self {
            Self::Success(s) => s.clone(),
            Self::Error(e) => e.clone(),
            Self::Structured {
                stdout,
                stderr,
                exit_code,
                duration_ms,
                interrupted,
            } => {
                let mut parts = Vec::new();

                if let Some(code) = exit_code {
                    parts.push(format!("Exit code: {code}"));
                }

                parts.push(format!("Duration: {duration_ms}ms"));

                if stdout.is_empty() {
                    parts.push("stdout: (empty)".to_string());
                } else {
                    parts.push(format!("stdout:\n{stdout}"));
                }

                if stderr.is_empty() {
                    parts.push("stderr: (empty)".to_string());
                } else {
                    parts.push(format!("stderr:\n{stderr}"));
                }

                if *interrupted {
                    use crate::fsm::tools::InterruptReason;
                    let msg = match interrupt_reason {
                        Some(InterruptReason::Timeout(secs)) => {
                            format!("[Timed out after {secs}s]")
                        }
                        _ => "[Interrupted by user]".to_string(),
                    };
                    parts.push(msg);
                }

                parts.join("\n\n")
            }
        }
    }

    /// Whether this outcome represents an error.
    pub fn is_error(&self) -> bool {
        match self {
            Self::Error(_) => true,
            Self::Structured {
                exit_code: Some(code),
                ..
            } if *code != 0 => true,
            _ => false,
        }
    }
}

/// Cached VT100 preview data for a shell tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPreview {
    pub lines: Vec<String>,
    pub exit_code: Option<i32>,
    pub interrupted: Option<crate::fsm::tools::InterruptReason>,
}

/// A tool call from the server, with parsed input parameters.
#[derive(Debug, Clone)]
#[enum_dispatch(PermissibleToolCall)]
pub enum ClientToolCall {
    Read(ReadToolCall),
    Edit(EditToolCall),
    Write(WriteToolCall),
    Shell(ShellToolCall),
    AtuinHistory(AtuinHistoryToolCall),
    LoadSkill(LoadSkillToolCall),
}

impl TryFrom<(&str, &serde_json::Value)> for ClientToolCall {
    type Error = eyre::Error;

    fn try_from((name, input): (&str, &serde_json::Value)) -> Result<Self, Self::Error> {
        match name {
            "read_file" => Ok(Self::Read(ReadToolCall::try_from(input)?)),
            "edit_file" => Ok(Self::Edit(EditToolCall::try_from(input)?)),
            "write_file" => Ok(Self::Write(WriteToolCall::try_from(input)?)),
            "execute_shell_command" => Ok(Self::Shell(ShellToolCall::try_from(input)?)),
            "atuin_history" => Ok(Self::AtuinHistory(AtuinHistoryToolCall::try_from(input)?)),
            "load_skill" => Ok(Self::LoadSkill(LoadSkillToolCall::try_from(input)?)),
            _ => Err(eyre::eyre!("Unknown tool call: {name}")),
        }
    }
}

impl ClientToolCall {
    pub(crate) fn descriptor(&self) -> &'static descriptor::ToolDescriptor {
        match self {
            Self::Read(_) => descriptor::READ,
            Self::Edit(_) => descriptor::EDIT,
            Self::Write(_) => descriptor::WRITE,
            Self::Shell(_) => descriptor::SHELL,
            Self::AtuinHistory(_) => descriptor::ATUIN_HISTORY,
            Self::LoadSkill(_) => descriptor::LOAD_SKILL,
        }
    }

    /// The permission rule name for this tool category.
    ///
    /// Edit and Write share the `"Write"` rule name — a Write permission
    /// covers both str_replace edits and full file creates. Write also
    /// implies Read (checked in `ReadToolCall::matches_rule`).
    pub(crate) fn rule_name(&self) -> &'static str {
        match self {
            Self::Read(_) => "Read",
            Self::Edit(_) => "Write",
            Self::Write(_) => "Write",
            Self::Shell(_) => "Shell",
            Self::AtuinHistory(_) => "AtuinHistory",
            Self::LoadSkill(_) => "LoadSkill",
        }
    }

    /// The resolved file path for this tool call, if it's a file-based tool.
    /// Used to build scoped permission rules like `Write(/abs/path/to/file)`.
    pub(crate) fn resolved_file_path(&self) -> Option<PathBuf> {
        match self {
            Self::Read(tool) => Some(tool.resolved_path()),
            Self::Edit(tool) => Some(tool.resolved_path()),
            Self::Write(tool) => Some(tool.resolved_path()),
            Self::Shell(_) | Self::AtuinHistory(_) | Self::LoadSkill(_) => None,
        }
    }
}

/// A trait for tool calls that can be checked against permission rules.
#[enum_dispatch]
pub trait PermissibleToolCall {
    /// Checks if this tool call matches the given permission rule.
    fn matches_rule(&self, rule: &Rule) -> bool;

    /// Check if every part of this tool call is covered by at least one rule in
    /// the set.  For compound operations (e.g. shell pipelines), each sub-part
    /// must be individually covered.  The default treats the call as atomic —
    /// any single matching rule is sufficient.
    fn all_covered_by(&self, rules: &[Rule]) -> bool {
        rules.iter().any(|r| self.matches_rule(r))
    }

    /// The directory whose permission files apply to this call, and their parents': for a file
    /// tool, the directory of the resolved file.
    fn target_dir(&self) -> Option<PathBuf> {
        None
    }
}

/// Expand shell constructs (`~`, `$HOME`, etc.) in a path string.
///
/// Tool call paths arrive as raw strings from the API without shell
/// expansion.
pub fn expand_path(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).into_owned())
}

#[derive(Debug, Clone)]
pub struct ReadToolCall {
    pub path: PathBuf,
    pub offset: u64,
    pub limit: u64,
}

impl TryFrom<&serde_json::Value> for ReadToolCall {
    type Error = eyre::Error;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        let path =
            value.get("file_path").and_then(|v| v.as_str()).ok_or(eyre::eyre!("Missing path"))?;

        let offset = value.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
        let limit = value
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_FILE_READ_LINES)
            .min(MAX_FILE_READ_LINES);

        Ok(Self {
            path: expand_path(path),
            offset,
            limit,
        })
    }
}

impl ReadToolCall {
    pub fn resolved_path(&self) -> PathBuf {
        resolve_tool_path(&self.path)
    }

    #[must_use]
    pub fn execute(&self) -> ToolOutcome {
        let path = self.resolved_path();

        if !path.exists() {
            return ToolOutcome::Error(format!("Error: file does not exist: {}", path.display()));
        }

        if path.is_dir() {
            let Some(files) = std::fs::read_dir(&path).ok().and_then(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .into()
            }) else {
                return ToolOutcome::Error(format!(
                    "Error: could not read directory: {}",
                    path.display()
                ));
            };

            return ToolOutcome::Success(format!("Directory contents:\n{}", files.join("\n")));
        }

        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) => return ToolOutcome::Error(format!("Error opening file: {e}")),
        };
        let reader = std::io::BufReader::new(file);

        let raw_lines = reader
            .lines()
            .skip(usize::conv(self.offset))
            .take(usize::conv(self.limit))
            .collect::<Result<Vec<_>, _>>();

        match raw_lines {
            Ok(lines) => {
                let first_line_no = usize::conv(self.offset) + 1;
                let last_line_no = first_line_no + lines.len().saturating_sub(1);
                let width = usize::conv(last_line_no.max(1).ilog10()) + 1;

                let numbered: String = lines
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>width$}\t{line}", first_line_no + i))
                    .collect::<Vec<_>>()
                    .join("\n");

                if numbered.len() > 100_000 {
                    ToolOutcome::Error(format!(
                        "Error: file is too large to read ({} bytes in {} lines); use read_file \
                         with offset and a smaller limit to read a subset of the file",
                        numbered.len(),
                        lines.len()
                    ))
                } else {
                    ToolOutcome::Success(numbered)
                }
            }
            Err(e) => ToolOutcome::Error(format!("Error reading file: {e}")),
        }
    }
}

impl PermissibleToolCall for ReadToolCall {
    fn target_dir(&self) -> Option<PathBuf> {
        permission_dir(self.resolved_path())
    }

    fn matches_rule(&self, rule: &Rule) -> bool {
        // Write implies Read — a Write permission on a path also permits reading it.
        if rule.tool != "Read" && rule.tool != "Write" {
            return false;
        }

        match rule.scope.as_deref() {
            None | Some("*") => true,
            Some(scope) => file_matches_scope(&self.path, scope),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EditToolCall {
    pub path: PathBuf,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
}

impl TryFrom<&serde_json::Value> for EditToolCall {
    type Error = eyre::Error;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        let path = value
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or(eyre::eyre!("Missing file_path"))?;

        let old_string = value
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or(eyre::eyre!("Missing old_string"))?;

        let new_string = value
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or(eyre::eyre!("Missing new_string"))?;

        let replace_all = value.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);

        Ok(Self {
            path: expand_path(path),
            old_string: old_string.to_string(),
            new_string: new_string.to_string(),
            replace_all,
        })
    }
}

impl EditToolCall {
    /// Resolve the edit path to an absolute path.
    pub fn resolved_path(&self) -> PathBuf {
        resolve_tool_path(&self.path)
    }

    /// Execute the edit against the filesystem.
    ///
    /// Checks freshness via the provided tracker, validates matches, applies
    /// the replacement, and writes atomically. Returns the outcome and (on
    /// success) the new file content bytes for tracker updates.
    ///
    /// Callers should snapshot the file before calling this method and
    /// update the file tracker after a successful return.
    #[must_use]
    pub fn execute(
        &self,
        resolved_path: &Path,
        file_tracker: &crate::file_tracker::FileReadTracker,
    ) -> (ToolOutcome, Option<Vec<u8>>) {
        // 1. Basic validation
        if !resolved_path.exists() {
            return (
                ToolOutcome::Error(format!(
                    "Error: file does not exist: {}",
                    resolved_path.display()
                )),
                None,
            );
        }
        if resolved_path.is_dir() {
            return (
                ToolOutcome::Error(format!(
                    "Error: path is a directory, not a file: {}",
                    resolved_path.display()
                )),
                None,
            );
        }
        if self.old_string.is_empty() {
            return (
                ToolOutcome::Error(
                    "old_string must not be empty. To create a new file, use write_file instead."
                        .to_string(),
                ),
                None,
            );
        }

        // 2. Freshness check
        if let Some(error) = unseen_changes(file_tracker, resolved_path) {
            return (error, None);
        }

        // 3. Read current contents
        let content = match std::fs::read_to_string(resolved_path) {
            Ok(c) => c,
            Err(e) => return (ToolOutcome::Error(format!("Error reading file: {e}")), None),
        };

        // 4. Find and validate matches
        let match_count = content.matches(&self.old_string).count();

        if match_count == 0 {
            return (
                ToolOutcome::Error(format!(
                    "old_string not found in {}. Make sure it matches exactly, including \
                     whitespace and indentation.",
                    resolved_path.display()
                )),
                None,
            );
        }

        if match_count > 1 && !self.replace_all {
            return (
                ToolOutcome::Error(format!(
                    "Found {match_count} matches of old_string in {}, but replace_all is false. \
                     Either provide more context to make the match unique, or set replace_all to \
                     true.",
                    resolved_path.display()
                )),
                None,
            );
        }

        // 5. Apply replacement
        let new_content = if self.replace_all {
            content.replace(&self.old_string, &self.new_string)
        } else {
            content.replacen(&self.old_string, &self.new_string, 1)
        };

        // 6. Write atomically
        let new_bytes = new_content.into_bytes();
        if let Err(e) = crate::snapshots::atomic_write_file(resolved_path, &new_bytes) {
            return (ToolOutcome::Error(format!("Error writing file: {e}")), None);
        }

        // 7. Success
        let verb = if match_count == 1 {
            "occurrence"
        } else {
            "occurrences"
        };
        (
            ToolOutcome::Success(format!(
                "Edited {}: replaced {match_count} {verb} of old_string with new_string.",
                resolved_path.display()
            )),
            Some(new_bytes),
        )
    }
}

impl PermissibleToolCall for EditToolCall {
    fn target_dir(&self) -> Option<PathBuf> {
        permission_dir(self.resolved_path())
    }

    fn matches_rule(&self, rule: &Rule) -> bool {
        if rule.tool != "Write" {
            return false;
        }

        match rule.scope.as_deref() {
            None | Some("*") => true,
            Some(scope) => file_matches_scope(&self.path, scope),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriteToolCall {
    pub path: PathBuf,
    pub content: String,
    pub overwrite: bool,
}

impl TryFrom<&serde_json::Value> for WriteToolCall {
    type Error = eyre::Error;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        let path = value
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or(eyre::eyre!("Missing file_path"))?;

        let content =
            value.get("content").and_then(|v| v.as_str()).ok_or(eyre::eyre!("Missing content"))?;

        let overwrite = value.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);

        Ok(Self {
            path: expand_path(path),
            content: content.to_string(),
            overwrite,
        })
    }
}

/// The error for changing an existing file the model hasn't read, or has read before it last
/// changed: either way, the change could discard content the model never saw.
fn unseen_changes(
    file_tracker: &crate::file_tracker::FileReadTracker,
    path: &Path,
) -> Option<ToolOutcome> {
    use crate::file_tracker::FreshnessCheck;

    let message = match file_tracker.check_freshness(path) {
        Ok(FreshnessCheck::Fresh) => return None,
        Ok(FreshnessCheck::NotRead) => {
            "File has not been read yet. Read it first before changing it.".to_owned()
        }
        Ok(FreshnessCheck::Stale) => "File has been modified since read, either by the user or by \
                                      a linter. Read it again before attempting to change it."
            .to_owned(),
        Err(e) => format!("Error checking file state: {e}"),
    };
    Some(ToolOutcome::Error(message))
}

impl WriteToolCall {
    /// Resolve the write path to an absolute path.
    pub fn resolved_path(&self) -> PathBuf {
        resolve_tool_path(&self.path)
    }

    /// Execute the write operation.
    ///
    /// Creates a new file or overwrites an existing one (if `overwrite` is set) that the model has
    /// read since it last changed. Returns the outcome and the written bytes (for tracker updates).
    #[must_use]
    pub fn execute(
        &self,
        resolved_path: &Path,
        file_tracker: &crate::file_tracker::FileReadTracker,
    ) -> (ToolOutcome, Option<Vec<u8>>) {
        if resolved_path.is_dir() {
            return (
                ToolOutcome::Error(format!(
                    "Error: path is a directory, not a file: {}",
                    resolved_path.display()
                )),
                None,
            );
        }
        if resolved_path.exists() && !self.overwrite {
            return (
                ToolOutcome::Error(format!(
                    "File already exists: {}. Set overwrite to true to replace it, or use \
                     edit_file to make targeted changes.",
                    resolved_path.display()
                )),
                None,
            );
        }

        // Capture before the write — after atomic_write the file always exists.
        let existed = resolved_path.exists();
        if existed && let Some(error) = unseen_changes(file_tracker, resolved_path) {
            return (error, None);
        }

        // Write atomically
        let content_bytes = self.content.as_bytes().to_vec();
        if let Err(e) = crate::snapshots::atomic_write_file(resolved_path, &content_bytes) {
            return (ToolOutcome::Error(format!("Error writing file: {e}")), None);
        }

        let line_count = self.content.lines().count();
        let verb = if existed {
            "Overwrote"
        } else {
            "Created"
        };
        (
            ToolOutcome::Success(format!(
                "{verb} {} ({line_count} lines).",
                resolved_path.display()
            )),
            Some(content_bytes),
        )
    }
}

impl PermissibleToolCall for WriteToolCall {
    fn target_dir(&self) -> Option<PathBuf> {
        permission_dir(self.resolved_path())
    }

    fn matches_rule(&self, rule: &Rule) -> bool {
        if rule.tool != "Write" {
            return false;
        }

        match rule.scope.as_deref() {
            None | Some("*") => true,
            Some(scope) => file_matches_scope(&self.path, scope),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShellToolCall {
    pub dir: Option<PathBuf>,
    pub command: String,
    pub shell: String,
    /// Maximum execution time in seconds (from LLM). Clamped to 1..=600, default 30.
    pub timeout_secs: u64,
    // allow dead code here; this will be tied into o11y and user-facing descriptions
    #[expect(dead_code)]
    pub description: Option<String>,
}

impl TryFrom<&serde_json::Value> for ShellToolCall {
    type Error = eyre::Error;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        let dir = value.get("dir").and_then(|v| v.as_str());

        let command =
            value.get("command").and_then(|v| v.as_str()).ok_or(eyre::eyre!("Missing command"))?;

        let shell = value.get("shell").and_then(|v| v.as_str()).unwrap_or("bash").to_string();

        let timeout_secs =
            value.get("timeout").and_then(|v| v.as_u64()).filter(|&v| v > 0).unwrap_or(30).min(600);

        let description = value.get("description").and_then(|v| v.as_str()).map(|s| s.to_string());

        Ok(Self {
            dir: dir.map(expand_path),
            command: command.to_string(),
            shell,
            timeout_secs,
            description,
        })
    }
}

impl PermissibleToolCall for ShellToolCall {
    fn target_dir(&self) -> Option<PathBuf> {
        self.dir.as_deref().map(resolve_tool_path)
    }

    fn matches_rule(&self, rule: &Rule) -> bool {
        if rule.tool != "Shell" {
            return false;
        }

        let Some(scope) = rule.scope.as_ref() else {
            // Shell without scope matches all shell commands
            return true;
        };

        let shell_kind = crate::permissions::shell::ShellKind::from_shell_name(&self.shell);
        let parsed = crate::permissions::shell::parse_shell_command(&self.command, shell_kind);
        // Deny/ask path: prefix_bare = true so `deny = ["Shell(rm)"]` blocks `rm -rf /`. Either
        // spelling of a command matches: `git * --amend` catches `git commit --amend > log`.
        let redirected: Vec<_> = parsed
            .subcommands
            .iter()
            .map(crate::permissions::shell::ShellCommand::as_redirected)
            .collect();
        crate::permissions::shell::any_subcommand_matches(&parsed.subcommands, true, scope)
            || crate::permissions::shell::any_subcommand_matches(&redirected, true, scope)
    }

    /// For compound shell commands, every subcommand must be individually
    /// covered by at least one rule.  This ensures that `allow = ["Shell(git *)"]`
    /// does not silently permit `git add . && rm -rf /`.
    fn all_covered_by(&self, rules: &[Rule]) -> bool {
        use crate::permissions::shell;

        let allows_everything = rules
            .iter()
            .any(|rule| rule.tool == "Shell" && matches!(rule.scope.as_deref(), None | Some("*")));
        if allows_everything {
            return true;
        }

        let shell_kind = shell::ShellKind::from_shell_name(&self.shell);
        let parsed = shell::parse_shell_command(&self.command, shell_kind);

        // If parsing yields nothing, or may have missed a nested command, don't vacuously
        // allow: fall through to ask.
        !parsed.opaque
            && !parsed.subcommands.is_empty()
            && parsed.subcommands.iter().all(|subcmd| {
                rules.iter().any(|rule| {
                    if rule.tool != "Shell" {
                        return false;
                    }
                    match rule.scope.as_deref() {
                        None | Some("*") => true,
                        // Allow path: prefix_bare = false so `Shell(git commit)`
                        // only allows exactly `git commit`, not `git commit --amend`,
                        // and with its redirects, not `git commit > ~/.bashrc`.
                        Some(scope) => shell::any_subcommand_matches(
                            std::slice::from_ref(&subcmd.as_redirected()),
                            false,
                            scope,
                        ),
                    }
                })
            })
    }
}

/// Preview viewport height for VT100 emulation.
const PREVIEW_HEIGHT: NonZeroU16 = NonZeroU16::new(10).unwrap();

/// Default terminal width for VT100 emulation.
const PREVIEW_WIDTH: NonZeroU16 = NonZeroU16::new(120).unwrap();

/// Extract plain text lines from a VT100 screen buffer.
///
/// Strips trailing blank lines so the result only contains rows with actual
/// content. Without this, the fixed-size VT100 screen (PREVIEW_HEIGHT rows)
/// would always return that many lines, and downstream components that use
/// tail-mode display (like the Viewport) would show the blank padding rows
/// instead of the real output.
fn vt100_screen_lines(screen: &vt100::Screen) -> Vec<String> {
    let (rows, cols) = screen.size();
    let rows = rows.get();
    let cols = cols.get();

    let mut lines = Vec::with_capacity(usize::conv(rows));
    for row in 0..rows {
        let mut line = String::with_capacity(usize::conv(cols));
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                line.push_str(cell.contents());
            }
        }
        lines.push(line.trim_end().to_string());
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// How much of a command's raw output to keep, from the end, for its result.
const RAW_OUTPUT_LIMIT_BYTES: usize = 2 * ToolOutcome::MAX_STRUCTURED_OUTPUT_SIZE;

/// Append `chunk` to `buf`, keeping only the last [`RAW_OUTPUT_LIMIT_BYTES`]. Trimmed once it holds
/// twice that, so appending stays cheap.
fn push_tail(buf: &mut Vec<u8>, chunk: &[u8]) {
    buf.extend_from_slice(chunk);
    if buf.len() > 2 * RAW_OUTPUT_LIMIT_BYTES {
        buf.drain(..buf.len() - RAW_OUTPUT_LIMIT_BYTES);
    }
}

/// Execute a shell command with VT100 emulation and streaming output.
///
/// Feeds stdout+stderr into a `vt100::Parser` so that ANSI escape sequences,
/// progress bars (`\r`), and cursor movement are handled correctly. Periodically
/// sends the current screen state as `Vec<String>` through `output_tx` for the
/// live preview.
///
/// Captures the FULL stdout and stderr separately for the tool result sent to the LLM.
/// Returns a `ToolOutcome::Structured` with full output, exit code, and duration.
pub async fn execute_shell_command_streaming(
    shell_call: &ShellToolCall,
    output_tx: tokio::sync::mpsc::Sender<Vec<String>>,
    mut interrupt_rx: tokio::sync::oneshot::Receiver<()>,
) -> ToolOutcome {
    use tokio::io::AsyncReadExt;

    let start = std::time::Instant::now();

    // TODO: check if this is proper for all shells we support
    let mut cmd = tokio::process::Command::new(&shell_call.shell);
    cmd.arg("-c").arg(&shell_call.command);
    // The TUI owns the terminal: a command can't prompt on it.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    crate::process::own_group(&mut cmd);

    if let Some(ref dir) = shell_call.dir {
        cmd.current_dir(dir);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return ToolOutcome::Error(format!("Failed to spawn command: {e}")),
    };

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // VT100 emulator for the live preview (viewport-sized)
    let mut parser = vt100::Parser::new(PREVIEW_HEIGHT, PREVIEW_WIDTH, 0);

    let mut stdout_reader = tokio::io::BufReader::new(stdout);
    let mut stderr_reader = tokio::io::BufReader::new(stderr);

    let mut stdout_buf = [0u8; 4096];
    let mut stderr_buf = [0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;

    // Full output buffers (for the LLM, not the preview)
    let mut full_stdout = Vec::<u8>::new();
    let mut full_stderr = Vec::<u8>::new();

    let mut interval = tokio::time::interval(Duration::from_millis(50));

    // Send initial empty screen
    let initial_lines = vt100_screen_lines(parser.screen());
    let _ = output_tx.send(initial_lines).await;

    enum Step {
        Interrupt,
        Exited(std::io::Result<std::process::ExitStatus>),
        Stdout(std::io::Result<usize>),
        Stderr(std::io::Result<usize>),
        Tick,
    }

    let mut interrupted = false;
    let mut status = None;

    // Until the shell has exited and its output closed, either of which can come first: a
    // background job can hold the output open after the shell exits, and a command can close its
    // output and keep running. An interrupt stops it either way.
    while status.is_none() || !stdout_done || !stderr_done {
        let step = tokio::select! {
            biased;
            _ = &mut interrupt_rx, if !interrupted => Step::Interrupt,
            result = child.wait(), if status.is_none() => Step::Exited(result),
            result = stdout_reader.read(&mut stdout_buf), if !stdout_done => Step::Stdout(result),
            result = stderr_reader.read(&mut stderr_buf), if !stderr_done => Step::Stderr(result),
            _ = interval.tick() => Step::Tick,
        };

        match step {
            Step::Interrupt => {
                interrupted = true;
                crate::process::kill_tree(&mut child);
            }
            Step::Exited(result) => status = Some(result),
            Step::Stdout(Ok(0) | Err(_)) => stdout_done = true,
            Step::Stdout(Ok(n)) => {
                push_tail(&mut full_stdout, &stdout_buf[..n]);
                ansi::onlcr(&stdout_buf[..n]).for_each(|chunk| parser.process(chunk));
            }
            Step::Stderr(Ok(0) | Err(_)) => stderr_done = true,
            Step::Stderr(Ok(n)) => {
                push_tail(&mut full_stderr, &stderr_buf[..n]);
                // Feed stderr to the preview parser too, so it shows in the VT100 screen
                ansi::onlcr(&stderr_buf[..n]).for_each(|chunk| parser.process(chunk));
            }
            Step::Tick => {
                let lines = vt100_screen_lines(parser.screen());
                let _ = output_tx.send(lines).await;
            }
        }
    }

    let exit_code = match status {
        Some(Ok(status)) => status.code(),
        Some(Err(e)) if !interrupted => {
            return ToolOutcome::Error(format!("Failed to wait for command: {e}"));
        }
        Some(Err(_)) | None => None,
    };

    let duration = start.elapsed();

    // Send final screen state
    let final_lines = vt100_screen_lines(parser.screen());
    let _ = output_tx.send(final_lines).await;

    // Strip ANSI escape sequences for clean LLM output by running
    // the raw bytes through a VT100 parser and extracting plain text.
    let rows = PREVIEW_HEIGHT;
    let cols = PREVIEW_WIDTH;

    let [stdout_text, stderr_text] = [full_stdout, full_stderr].map(|output| {
        let mut text = ansi::to_plain_text(&output, rows, cols);
        let start = text.len().saturating_sub(ToolOutcome::MAX_STRUCTURED_OUTPUT_SIZE);
        let start = text.ceil_char_boundary(start);
        text.drain(..start);
        text
    });

    ToolOutcome::Structured {
        stdout: stdout_text,
        stderr: stderr_text,
        exit_code,
        duration_ms: u64::conv(duration.as_millis()),
        interrupted,
    }
}

#[derive(Debug, Clone)]
pub struct AtuinHistoryToolCall {
    pub filter_modes: Vec<HistorySearchFilterMode>,
    pub query: String,
    pub limit: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum HistorySearchFilterMode {
    Global,
    Host,
    Session,
    Directory,
    Workspace,
}

impl HistorySearchFilterMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

impl From<HistorySearchFilterMode> for history::FilterMode {
    fn from(mode: HistorySearchFilterMode) -> Self {
        match mode {
            HistorySearchFilterMode::Global => Self::Global,
            HistorySearchFilterMode::Host => Self::Host,
            HistorySearchFilterMode::Session => Self::Session,
            HistorySearchFilterMode::Directory => Self::Directory,
            HistorySearchFilterMode::Workspace => Self::Workspace,
        }
    }
}

/// `atuin_history`'s arguments as sent. An argument the schema doesn't list is an error rather
/// than ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryArgs {
    query: String,
    filter_modes: Option<Vec<String>>,
    limit: Option<i64>,
}

impl TryFrom<&serde_json::Value> for AtuinHistoryToolCall {
    type Error = eyre::Error;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        // Every field but `query` is optional, and JSON null counts as omitted because models
        // often send null for optional params. A missing scope means a global search — evals
        // showed that forcing the model to pick a scope on every call made it stop calling the
        // tool at all.
        let args = HistoryArgs::deserialize(value)?;

        let filter_modes = args
            .filter_modes
            .unwrap_or_default()
            .iter()
            .map(|mode| {
                mode.parse::<HistorySearchFilterMode>()
                    .map_err(|_| eyre::eyre!("Invalid filter mode: {mode}"))
            })
            .collect::<Result<Vec<HistorySearchFilterMode>>>()?;

        Ok(Self {
            filter_modes,
            query: args.query,
            limit: args.limit.unwrap_or(DEFAULT_HISTORY_RESULTS).clamp(1, MAX_HISTORY_RESULTS),
        })
    }
}

/// Top `results` up to `size` with `loose` matches for commands not already
/// in it, keeping their order.
fn fill_with_loose(results: &mut Vec<History>, loose: Vec<History>, size: usize) {
    for history in loose {
        if results.len() >= size {
            break;
        }
        if !results.iter().any(|r| r.command == history.command) {
            results.push(history);
        }
    }
}

impl PermissibleToolCall for AtuinHistoryToolCall {
    fn matches_rule(&self, rule: &Rule) -> bool {
        rule.tool == "AtuinHistory"
    }
}

impl AtuinHistoryToolCall {
    pub(crate) async fn execute(&self, db: &HistoryDb) -> ToolOutcome {
        let context = history::Context::current();

        let search_mode =
            self.filter_modes.first().copied().unwrap_or(HistorySearchFilterMode::Global);

        // An empty session would silently match nothing; error instead so a
        // missing $ATUIN_SESSION (a shell Atuin doesn't hook) isn't mistaken
        // for empty history.
        if matches!(search_mode, HistorySearchFilterMode::Session) && context.session.is_empty() {
            return ToolOutcome::Error(
                "Session-scoped search is unavailable: $ATUIN_SESSION is not set, so there is no \
                 shell session to scope to. Use another filter mode."
                    .to_string(),
            );
        }

        // Fetch one row beyond the requested limit so the truncation notice
        // appended to the results below can say "more exist" as a fact rather
        // than a guess.
        let fetch = self.limit + 1;

        // Each term as a substring first: fuzzy matching alone takes the most
        // recent commands with the letters anywhere in order, so a short query
        // like `pscale` is buried under long heredocs that happen to spell it
        // out. Fuzzy matches only fill what is left of the page.
        let search = |mode| db.search(mode, search_mode.into(), &context, &self.query, fetch);
        // Full-text results come back newest first; rank them as fuzzy ones
        // are, tightest match first.
        let mut results = match search(SearchMode::FullText).await {
            Ok(results) => history::rank_fuzzy(&self.query, results),
            Err(e) => return ToolOutcome::Error(format!("History search failed: {e}")),
        };
        // The clamp keeps this in 1..=MAX_HISTORY_RESULTS, so the conversion
        // never fails; the fallback only exists to keep it infallible.
        let page_size = usize::try_from(self.limit.clamp(1, MAX_HISTORY_RESULTS)).unwrap_or(1);
        let exact = results.len();
        // A page with room, or exactly full (to know whether more exist), is
        // topped up; an empty query matches the same either way.
        if exact <= page_size && history::has_fuzzy_terms(&self.query) {
            let loose = match search(SearchMode::Fuzzy).await {
                Ok(loose) => loose,
                Err(e) => return ToolOutcome::Error(format!("History search failed: {e}")),
            };
            fill_with_loose(&mut results, loose, page_size + 1);
        }
        let truncated = results.len() > page_size;
        results.truncate(page_size);

        if results.is_empty() {
            // An unadorned "no results" reads to the model as "this tool is
            // useless". List which search parameters are worth loosening so
            // its retry has somewhere to go.
            let mut hints = Vec::new();
            if !self.query.is_empty() {
                hints.push("query terms are AND-ed, so try fewer or shorter terms");
            }
            if !matches!(search_mode, HistorySearchFilterMode::Global) {
                hints.push("widen the scope to 'global'");
            }
            let mut msg = format!(
                "No history entries matched query {query:?} (scope: {scope}).",
                query = self.query,
                scope = search_mode.as_str(),
            );
            if !hints.is_empty() {
                msg.push_str(&format!(" To find more: {}.", hints.join("; ")));
            }
            return ToolOutcome::Success(msg);
        }

        let local_offset = crate::history_format::local_offset();

        let mut formatted: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(i, history)| {
                crate::history_format::format_history_search_result(i + 1, history, local_offset)
            })
            .collect();
        if exact < results.len() {
            let which = if exact == 0 {
                "No command contains the query as written; these"
            } else {
                "The rest"
            };
            formatted.insert(
                exact,
                format!(
                    "[{which} only match fuzzily: the query's letters appear in order, not \
                     necessarily together.]"
                ),
            );
        }

        if truncated {
            // The parser clamps `limit` to MAX_HISTORY_RESULTS, so a model
            // that retries with a larger limit at the cap would just get the
            // identical page back; only suggest raising it below the cap.
            let advice = if self.limit < MAX_HISTORY_RESULTS {
                "Refine the query or raise `limit` to see others."
            } else {
                "That is the maximum page size; refine the query to narrow them."
            };
            formatted
                .push(format!("[Showing the first {page_size} matches; more exist. {advice}]"));
        }

        ToolOutcome::Success(formatted.join("\n"))
    }
}

#[derive(Debug, Clone)]
pub struct LoadSkillToolCall {
    pub name: String,
}

impl TryFrom<&serde_json::Value> for LoadSkillToolCall {
    type Error = eyre::Error;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        let name =
            value.get("name").and_then(|v| v.as_str()).ok_or(eyre::eyre!("Missing skill name"))?;

        Ok(Self {
            name: name.to_string(),
        })
    }
}

impl PermissibleToolCall for LoadSkillToolCall {
    fn matches_rule(&self, rule: &Rule) -> bool {
        rule.tool == "LoadSkill"
    }
}

#[cfg(test)]
mod tests {
    use rstest::*;

    use super::*;

    fn read_rule(scope: Option<&str>) -> Rule {
        Rule {
            tool: "Read".to_string(),
            scope: scope.map(String::from),
        }
    }

    fn history(command: &str) -> History {
        History::for_tests(command)
    }

    fn commands(results: &[History]) -> Vec<&str> {
        results.iter().map(|h| h.command.as_str()).collect()
    }

    /// Substring matches lead; fuzzy ones fill the rest of the page without
    /// repeating a command.
    #[rstest]
    #[case::fills(3, vec!["pscale connect", "cat <<EOF p s c a l e", "pscale shell"])]
    #[case::stops_at_size(2, vec!["pscale connect", "cat <<EOF p s c a l e"])]
    #[case::exact_fills_the_page(1, vec!["pscale connect"])]
    fn fuzzy_matches_fill_after_substring_ones(#[case] size: usize, #[case] want: Vec<&str>) {
        let mut results = vec![history("pscale connect")];
        let loose = ["pscale connect", "cat <<EOF p s c a l e", "pscale shell", "pip scale"];
        fill_with_loose(&mut results, loose.into_iter().map(history).collect(), size);
        assert_eq!(commands(&results), want);
    }

    fn write_rule(scope: Option<&str>) -> Rule {
        Rule {
            tool: "Write".to_string(),
            scope: scope.map(String::from),
        }
    }

    fn read_tool(path: &str) -> ReadToolCall {
        ReadToolCall {
            path: expand_path(path),
            offset: 0,
            limit: 100,
        }
    }

    fn write_tool(path: &str) -> WriteToolCall {
        WriteToolCall {
            path: expand_path(path),
            content: String::new(),
            overwrite: false,
        }
    }

    // ── Cross-platform tests ──

    #[rstest]
    #[case::omitted(serde_json::json!({ "query": "cargo" }))]
    #[case::null(serde_json::json!({ "query": "cargo", "filter_modes": null }))]
    fn atuin_history_filter_modes_are_optional(#[case] input: serde_json::Value) {
        let call = AtuinHistoryToolCall::try_from(&input).unwrap();
        assert!(call.filter_modes.is_empty());
    }

    #[rstest]
    fn filter_mode_names_round_trip() {
        use strum::IntoEnumIterator;
        for mode in HistorySearchFilterMode::iter() {
            assert_eq!(mode.as_str().parse::<HistorySearchFilterMode>(), Ok(mode));
        }
    }

    #[rstest]
    fn atuin_history_filter_modes_reject_non_arrays() {
        let input = serde_json::json!({ "query": "cargo", "filter_modes": "global" });
        assert!(AtuinHistoryToolCall::try_from(&input).is_err());
    }

    #[rstest]
    fn atuin_history_rejects_unknown_arguments() {
        let input = serde_json::json!({ "query": "cargo", "since": "2h" });
        let err = AtuinHistoryToolCall::try_from(&input).unwrap_err().to_string();
        assert!(err.contains("`since`") && err.contains("`filter_modes`"), "{err}");
    }

    #[rstest]
    #[case::read_rule_none(read_rule(None), true)]
    #[case::write_implies_read(write_rule(None), true)]
    fn read_tool_rule_name(#[case] rule: Rule, #[case] expected: bool) {
        assert_eq!(read_tool("foo.txt").matches_rule(&rule), expected);
    }

    /// A project with `secret.txt` in it, a symlink to it, and a subdirectory. Resolved, so the
    /// paths match their resolved spelling on macOS, where the temp dir is under a symlink.
    #[cfg(unix)]
    fn project_with_secret() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = std::fs::canonicalize(dir.path()).expect("the temp dir resolves");
        std::fs::write(root.join("secret.txt"), "hunter2").expect("the secret is written");
        std::fs::create_dir(root.join("sub")).expect("the subdirectory is created");
        std::os::unix::fs::symlink(root.join("secret.txt"), root.join("link.txt"))
            .expect("the symlink is created");
        (dir, root)
    }

    #[cfg(unix)]
    #[rstest]
    #[case::direct("secret.txt")]
    #[case::dot_dot("sub/../secret.txt")]
    #[case::dot("./secret.txt")]
    #[case::symlink("link.txt")]
    fn a_deny_rule_catches_every_spelling_of_its_file(#[case] spelling: &str) {
        let (_dir, root) = project_with_secret();
        let deny = read_rule(Some(&root.join("secret.txt").to_string_lossy()));
        let call = read_tool(&root.join(spelling).to_string_lossy());
        assert!(call.matches_rule(&deny), "{spelling}");
    }

    #[cfg(unix)]
    #[rstest]
    fn file_tools_resolve_their_target() {
        let (_dir, root) = project_with_secret();
        let through_link = read_tool(&root.join("sub/../link.txt").to_string_lossy());
        assert_eq!(through_link.resolved_path(), root.join("secret.txt"));
        assert_eq!(through_link.target_dir(), Some(root.clone()));

        let new_file = write_tool(&root.join("sub/./new/../new.txt").to_string_lossy());
        assert_eq!(new_file.resolved_path(), root.join("sub/new.txt"));
    }

    /// `read_file` lists a directory, so that directory's own permission files apply.
    #[cfg(unix)]
    #[rstest]
    fn reading_a_directory_starts_at_its_own_permissions() {
        let (_dir, root) = project_with_secret();
        assert_eq!(
            read_tool(&root.join("sub").to_string_lossy()).target_dir(),
            Some(root.join("sub"))
        );
    }

    /// `link/../new.txt` is beside the link's target, as the kernel resolves it, not beside the
    /// link.
    #[cfg(unix)]
    #[rstest]
    fn a_new_file_after_a_symlinked_dir_lands_beside_its_target() {
        let (_dir, root) = project_with_secret();
        std::fs::create_dir_all(root.join("data/sub")).unwrap();
        std::os::unix::fs::symlink(root.join("data/sub"), root.join("sub/link")).unwrap();

        let call = write_tool(&root.join("sub/link/../new.txt").to_string_lossy());
        assert_eq!(call.resolved_path(), root.join("data/new.txt"));
    }

    #[rstest]
    #[case::write_rule_none(write_rule(None), true)]
    #[case::read_does_not_imply_write(read_rule(None), false)]
    fn write_tool_rule_name(#[case] rule: Rule, #[case] expected: bool) {
        assert_eq!(write_tool("foo.txt").matches_rule(&rule), expected);
    }

    #[rstest]
    #[case::edit_uses_write(write_rule(None), true)]
    #[case::edit_rejects_read(read_rule(None), false)]
    fn edit_tool_rule_name(#[case] rule: Rule, #[case] expected: bool) {
        let edit = EditToolCall {
            path: expand_path("/home/user/config.toml"),
            old_string: "x".into(),
            new_string: "y".into(),
            replace_all: false,
        };
        assert_eq!(edit.matches_rule(&rule), expected);
    }

    #[rstest]
    #[case::wildcard_star("foo/bar.rs", "*", true)]
    #[case::extension_glob_matches("notes.md", "*.md", true)]
    #[case::extension_glob_rejects("notes.txt", "*.md", false)]
    #[cfg_attr(
        unix,
        case::unix_absolute_glob_matches("/home/user/src/main.rs", "/home/user/src/*.rs", true)
    )]
    #[cfg_attr(
        unix,
        case::unix_absolute_glob_rejects(
            "/home/user/docs/readme.md",
            "/home/user/src/*.rs",
            false
        )
    )]
    #[cfg_attr(
        unix,
        case::unix_double_star_matches(
            "/project/crates/foo/src/lib.rs",
            "/project/crates/**/*.rs",
            true
        )
    )]
    #[cfg_attr(
        unix,
        case::unix_double_star_rejects(
            "/project/crates/foo/src/lib.py",
            "/project/crates/**/*.rs",
            false
        )
    )]
    #[cfg_attr(
        windows,
        case::windows_absolute_glob_matches(
            r"C:\Users\dev\src\main.rs",
            "C:/Users/dev/src/*.rs",
            true
        )
    )]
    #[cfg_attr(
        windows,
        case::windows_absolute_glob_rejects(
            r"C:\Users\dev\docs\readme.md",
            "C:/Users/dev/src/*.rs",
            false
        )
    )]
    #[cfg_attr(
        windows,
        case::windows_double_star_matches(
            r"C:\project\crates\foo\src\lib.rs",
            "C:/project/crates/**/*.rs",
            true
        )
    )]
    #[cfg_attr(
        windows,
        case::windows_double_star_rejects(
            r"C:\project\crates\foo\src\lib.py",
            "C:/project/crates/**/*.rs",
            false
        )
    )]
    fn read_scope_glob(#[case] path: &str, #[case] scope: &str, #[case] expected: bool) {
        assert_eq!(read_tool(path).matches_rule(&read_rule(Some(scope))), expected);
    }

    #[rstest]
    #[case("crates/**/*.rs", true)]
    #[case("crates/**/*.py", false)]
    fn relative_multi_segment_glob(#[case] scope: &str, #[case] expected: bool) {
        // This matches against the path relative to cwd
        let cwd = std::env::current_dir().unwrap();
        let abs = cwd.join("crates").join("atuin-ai").join("src").join("lib.rs");
        let tool = read_tool(abs.to_str().unwrap());
        assert_eq!(tool.matches_rule(&read_rule(Some(scope))), expected);
    }

    // ── all_covered_by tests (compound shell command semantics) ──

    fn shell_rule(scope: Option<&str>) -> Rule {
        Rule {
            tool: "Shell".to_string(),
            scope: scope.map(String::from),
        }
    }

    fn shell_tool(command: &str) -> ShellToolCall {
        ShellToolCall {
            dir: None,
            command: command.to_string(),
            shell: "bash".to_string(),
            timeout_secs: 30,
            description: None,
        }
    }

    #[rstest]
    #[case::git_scope_allows(vec![shell_rule(Some("git *"))], "git add .", true)]
    #[case::git_scope_rejects_npm(vec![shell_rule(Some("git *"))], "npm test", false)]
    #[case::compound_all_covered(
        vec![shell_rule(Some("git *")), shell_rule(Some("npm *"))],
        "git add . && npm test",
        true
    )]
    #[case::compound_partially_covered(
        vec![shell_rule(Some("git *"))],
        "git add . && npm test",
        false
    )]
    #[case::unscoped_covers_all(vec![shell_rule(None)], "git add . && rm -rf /", true)]
    #[case::wildcard_covers_all(vec![shell_rule(Some("*"))], "git add . && npm test", true)]
    fn shell_all_covered_by(
        #[case] rules: Vec<Rule>,
        #[case] command: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(shell_tool(command).all_covered_by(&rules), expected);
    }

    #[rstest]
    #[case::newline("git status\nrm -rf x")]
    #[case::background("git status & rm -rf x")]
    #[case::substitution("git status $(rm -rf x)")]
    #[case::expansion_default("git status ${x:-$(rm -rf x)}")]
    #[case::quoted_substitution("git commit -m \"$(rm -rf x)\"")]
    fn an_allow_rule_does_not_cover_a_hidden_command(
        #[values("bash", "fish", "nu")] shell: &str,
        #[case] command: &str,
    ) {
        let call = ShellToolCall {
            shell: shell.to_owned(),
            ..shell_tool(command)
        };
        assert!(!call.all_covered_by(&[shell_rule(Some("git *"))]), "{shell}: {command:?}");
    }

    /// Fish has no backtick substitution: there, the backticks are part of the arguments.
    #[rstest]
    fn an_allow_rule_does_not_cover_a_backtick_substitution(#[values("bash", "nu")] shell: &str) {
        let call = ShellToolCall {
            shell: shell.to_owned(),
            ..shell_tool("git status `rm -rf x`")
        };
        assert!(!call.all_covered_by(&[shell_rule(Some("git *"))]), "{shell}");
    }

    #[rstest]
    fn an_unscoped_allow_rule_covers_an_unreadable_command(
        #[values("bash", "fish", "nu")] shell: &str,
    ) {
        let call = ShellToolCall {
            shell: shell.to_owned(),
            ..shell_tool("git status $(rm -rf x) ${unclosed")
        };
        assert!(call.all_covered_by(&[shell_rule(None)]), "{shell}");
        assert!(call.all_covered_by(&[shell_rule(Some("*"))]), "{shell}");
    }

    #[rstest]
    #[case::write("git status > ~/.bashrc")]
    #[case::append("git status >> ~/.bashrc")]
    #[case::inside("git status > ~/.bashrc --short")]
    fn an_exact_allow_rule_does_not_cover_a_redirect(
        #[values("bash", "fish", "nu")] shell: &str,
        #[case] command: &str,
    ) {
        let call = ShellToolCall {
            shell: shell.to_owned(),
            ..shell_tool(command)
        };
        assert!(!call.all_covered_by(&[shell_rule(Some("git status"))]), "{shell}: {command}");
    }

    #[rstest]
    #[case::trailing("git commit --amend > /dev/null")]
    #[case::stderr("git commit --amend 2>&1")]
    fn a_deny_rule_still_matches_a_redirected_command(
        #[values("bash", "fish", "nu")] shell: &str,
        #[case] command: &str,
    ) {
        let call = ShellToolCall {
            shell: shell.to_owned(),
            ..shell_tool(command)
        };
        assert!(call.matches_rule(&shell_rule(Some("git * --amend"))), "{shell}: {command}");
    }

    #[rstest]
    fn an_allow_rule_covers_a_redirect(#[values("bash", "fish", "nu")] shell: &str) {
        let call = ShellToolCall {
            shell: shell.to_owned(),
            ..shell_tool("git log --oneline 2>&1")
        };
        assert!(call.all_covered_by(&[shell_rule(Some("git *"))]), "{shell}");
    }

    #[rstest]
    fn all_covered_by_non_shell_tool_unchanged() {
        // Non-shell tools use the default (any single rule matches)
        let rules = vec![read_rule(Some("*.md"))];
        assert!(read_tool("notes.md").all_covered_by(&rules));
        assert!(!read_tool("notes.txt").all_covered_by(&rules));
    }

    #[rstest]
    fn matches_rule_still_uses_any_semantics() {
        // matches_rule (used for deny/ask) still triggers on any subcommand
        let rule = shell_rule(Some("rm *"));
        assert!(shell_tool("git add . && rm -rf /").matches_rule(&rule));
    }

    #[rstest]
    fn bare_pattern_asymmetry() {
        // Deny (matches_rule, prefix_bare=true): bare "rm" blocks "rm -rf /"
        let deny_rule = shell_rule(Some("rm"));
        assert!(shell_tool("rm -rf /").matches_rule(&deny_rule));

        // Allow (all_covered_by, prefix_bare=false): bare "rm" only allows exactly "rm"
        let allow_rules = vec![shell_rule(Some("rm"))];
        assert!(shell_tool("rm").all_covered_by(&allow_rules));
        assert!(!shell_tool("rm -rf /").all_covered_by(&allow_rules));

        // Bare prefix match is word-boundary, not substring — "rm" must not match "rmbackup"
        assert!(!shell_tool("rmbackup").matches_rule(&deny_rule));
        assert!(!shell_tool("rmbackup /tmp").matches_rule(&deny_rule));
    }

    // ── edit_file execution tests ──

    mod edit {
        use super::*;
        use crate::file_tracker::FileReadTracker;

        /// Helper: create a temp file (with a closed handle), record it in a tracker.
        /// Returns the TempDir (keeps the path alive) and tracker.
        /// The file handle is closed so atomic_write_file can rename over it on Windows.
        fn setup_tracked_file(content: &str) -> (tempfile::TempDir, PathBuf, FileReadTracker) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test_file.toml");
            std::fs::write(&path, content).unwrap();

            let file_content = std::fs::read(&path).unwrap();
            let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

            let mut tracker = FileReadTracker::default();
            tracker.record_read(path.clone(), &file_content, mtime);

            (dir, path, tracker)
        }

        fn edit_call(path: &Path, old: &str, new: &str, replace_all: bool) -> EditToolCall {
            EditToolCall {
                path: path.to_path_buf(),
                old_string: old.to_string(),
                new_string: new.to_string(),
                replace_all,
            }
        }

        #[rstest]
        #[case::single(
            "[section]\nkey = old_value\n",
            "old_value",
            "new_value",
            false,
            "[section]\nkey = new_value\n",
            None
        )]
        #[case::replace_all(
            "aaa bbb aaa ccc aaa",
            "aaa",
            "xxx",
            true,
            "xxx bbb xxx ccc xxx",
            Some("3 occurrences")
        )]
        #[case::multiline(
            "[section]\nkey1 = val1\nkey2 = val2\n[other]\n",
            "key1 = val1\nkey2 = val2",
            "key1 = new1\nkey2 = new2",
            false,
            "[section]\nkey1 = new1\nkey2 = new2\n[other]\n",
            None
        )]
        fn edit_success(
            #[case] content: &str,
            #[case] old: &str,
            #[case] new: &str,
            #[case] replace_all: bool,
            #[case] expected: &str,
            #[case] success_substr: Option<&str>,
        ) {
            let (_dir, path, tracker) = setup_tracked_file(content);

            let call = edit_call(&path, old, new, replace_all);
            let (outcome, new_bytes) = call.execute(&path, &tracker);

            assert!(matches!(outcome, ToolOutcome::Success(_)));
            if let Some(s) = success_substr {
                assert!(matches!(outcome, ToolOutcome::Success(ref out) if out.contains(s)));
            }
            assert!(new_bytes.is_some());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        }

        #[rstest]
        fn error_file_not_read() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("unread.txt");
            std::fs::write(&path, "content").unwrap();
            let tracker = FileReadTracker::default(); // empty — never read

            let call = edit_call(&path, "x", "y", false);
            let (outcome, new_bytes) = call.execute(&path, &tracker);

            assert!(new_bytes.is_none());
            match outcome {
                ToolOutcome::Error(msg) => {
                    assert!(msg.contains("not been read yet"), "got: {msg}");
                }
                _ => panic!("expected error"),
            }
        }

        #[rstest]
        fn error_file_modified_since_read() {
            let (_dir, path, tracker) = setup_tracked_file("original");

            // Modify the file after the read was recorded
            std::thread::sleep(std::time::Duration::from_millis(10));
            std::fs::write(&path, "modified externally").unwrap();

            let call = edit_call(&path, "original", "replaced", false);
            let (outcome, new_bytes) = call.execute(&path, &tracker);

            assert!(new_bytes.is_none());
            match outcome {
                ToolOutcome::Error(msg) => {
                    assert!(msg.contains("modified since read"), "got: {msg}");
                }
                _ => panic!("expected error"),
            }
        }

        #[rstest]
        #[case::no_match("hello world", "nonexistent", "replacement", false, &["not found"], false)]
        #[case::multiple_without_replace_all(
            "foo bar foo baz foo",
            "foo",
            "qux",
            false,
            &["3 matches", "replace_all"],
            true
        )]
        #[case::empty_old_string("content", "", "something", false, &[], true)]
        #[case::preserves_on_no_match(
            "[config]\nport = 8080\nhost = localhost\n",
            "port = 9090",
            "port = 3000",
            false,
            &[],
            true
        )]
        fn edit_error(
            #[case] content: &str,
            #[case] old: &str,
            #[case] new: &str,
            #[case] replace_all: bool,
            #[case] expect_substrings: &[&str],
            #[case] check_unchanged: bool,
        ) {
            let (_dir, path, tracker) = setup_tracked_file(content);

            let (outcome, new_bytes) =
                edit_call(&path, old, new, replace_all).execute(&path, &tracker);

            assert!(new_bytes.is_none());
            let ToolOutcome::Error(msg) = outcome else {
                panic!("expected error")
            };
            for s in expect_substrings {
                assert!(msg.contains(*s), "got: {msg}");
            }
            if check_unchanged {
                assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
            }
        }

        #[rstest]
        fn error_file_does_not_exist() {
            let tracker = FileReadTracker::default();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("nonexistent.txt");

            let call = edit_call(&path, "x", "y", false);
            let (outcome, new_bytes) = call.execute(&path, &tracker);

            assert!(new_bytes.is_none());
            match outcome {
                ToolOutcome::Error(msg) => {
                    assert!(msg.contains("does not exist"), "got: {msg}");
                }
                _ => panic!("expected error"),
            }
        }
    }

    // ── Integration tests: full edit lifecycle ──
    //
    // These exercise the cross-component flow that dispatch orchestrates:
    // FileReadTracker → SnapshotStore → EditToolCall.execute → tracker update

    mod edit_integration {
        use super::*;
        use crate::edit_permissions::EditPermissionCache;
        use crate::file_tracker::FileReadTracker;
        use crate::snapshots::SnapshotStore;

        /// Simulate a file read (what dispatch does after ReadToolCall.execute).
        fn simulate_read(tracker: &mut FileReadTracker, path: &std::path::Path) {
            let content = std::fs::read(path).unwrap();
            let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
            tracker.record_read(path.to_path_buf(), &content, mtime);
        }

        /// Simulate a tracker update after edit (what dispatch does after execute).
        fn simulate_tracker_update(
            tracker: &mut FileReadTracker,
            path: &std::path::Path,
            new_bytes: &[u8],
        ) {
            let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
            tracker.update_after_edit(path, new_bytes, mtime);
        }

        #[rstest]
        fn full_read_snapshot_edit_cycle() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("config.toml");
            std::fs::write(&file_path, "[db]\nhost = localhost\nport = 5432\n").unwrap();

            let snapshot_dir = dir.path().join("snapshots").join("session-1");
            let mut tracker = FileReadTracker::default();
            let mut store = SnapshotStore::open(snapshot_dir.clone()).unwrap();

            // 1. Simulate reading the file
            simulate_read(&mut tracker, &file_path);

            // 2. Snapshot before edit
            let original = std::fs::read(&file_path).unwrap();
            store.ensure_snapshot(&file_path, &original).unwrap();

            // 3. Execute edit
            let call = EditToolCall {
                path: file_path.clone(),
                old_string: "host = localhost".to_string(),
                new_string: "host = 10.0.0.1".to_string(),
                replace_all: false,
            };
            let (outcome, new_bytes) = call.execute(&file_path, &tracker);
            assert!(matches!(outcome, ToolOutcome::Success(_)));
            let new_bytes = new_bytes.unwrap();

            // 4. Update tracker (simulating what dispatch does)
            simulate_tracker_update(&mut tracker, &file_path, &new_bytes);

            // Verify: file was edited
            assert_eq!(
                std::fs::read_to_string(&file_path).unwrap(),
                "[db]\nhost = 10.0.0.1\nport = 5432\n"
            );

            // Verify: snapshot has original content
            assert!(store.has_snapshot(&file_path));
            let snapshot_name = crate::snapshots::sanitize_path(&file_path);
            let snapshot_content =
                std::fs::read_to_string(snapshot_dir.join(snapshot_name)).unwrap();
            assert_eq!(snapshot_content, "[db]\nhost = localhost\nport = 5432\n");
        }

        #[rstest]
        fn second_edit_without_reread() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("config.toml");
            std::fs::write(&file_path, "key1 = aaa\nkey2 = bbb\n").unwrap();

            let mut tracker = FileReadTracker::default();

            // Read the file
            simulate_read(&mut tracker, &file_path);

            // First edit
            let call1 = EditToolCall {
                path: file_path.clone(),
                old_string: "key1 = aaa".to_string(),
                new_string: "key1 = xxx".to_string(),
                replace_all: false,
            };
            let (outcome, new_bytes) = call1.execute(&file_path, &tracker);
            assert!(matches!(outcome, ToolOutcome::Success(_)));
            simulate_tracker_update(&mut tracker, &file_path, &new_bytes.unwrap());

            // Second edit — should work without re-reading because tracker was updated
            let call2 = EditToolCall {
                path: file_path.clone(),
                old_string: "key2 = bbb".to_string(),
                new_string: "key2 = yyy".to_string(),
                replace_all: false,
            };
            let (outcome, new_bytes) = call2.execute(&file_path, &tracker);
            assert!(matches!(outcome, ToolOutcome::Success(_)));
            assert!(new_bytes.is_some());
            assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "key1 = xxx\nkey2 = yyy\n");
        }

        #[rstest]
        fn external_modification_between_edits() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("config.toml");
            std::fs::write(&file_path, "value = original\n").unwrap();

            let mut tracker = FileReadTracker::default();
            simulate_read(&mut tracker, &file_path);

            // First edit succeeds
            let call1 = EditToolCall {
                path: file_path.clone(),
                old_string: "value = original".to_string(),
                new_string: "value = edited".to_string(),
                replace_all: false,
            };
            let (outcome, new_bytes) = call1.execute(&file_path, &tracker);
            assert!(matches!(outcome, ToolOutcome::Success(_)));
            simulate_tracker_update(&mut tracker, &file_path, &new_bytes.unwrap());

            // External modification (e.g., user edits the file)
            std::thread::sleep(std::time::Duration::from_millis(10));
            std::fs::write(&file_path, "value = user_changed\n").unwrap();

            // Second edit should fail (stale)
            let call2 = EditToolCall {
                path: file_path.clone(),
                old_string: "value = edited".to_string(),
                new_string: "value = second_edit".to_string(),
                replace_all: false,
            };
            let (outcome, new_bytes) = call2.execute(&file_path, &tracker);
            assert!(new_bytes.is_none());
            match outcome {
                ToolOutcome::Error(msg) => assert!(msg.contains("modified since read")),
                _ => panic!("expected stale error"),
            }

            // File should be unchanged (the user's edit preserved)
            assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "value = user_changed\n");
        }

        #[rstest]
        fn snapshot_only_created_once_per_file() {
            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("config.toml");
            std::fs::write(&file_path, "a = 1\nb = 2\n").unwrap();

            let snapshot_dir = dir.path().join("snapshots").join("session-1");
            let mut tracker = FileReadTracker::default();
            let mut store = SnapshotStore::open(snapshot_dir).unwrap();

            simulate_read(&mut tracker, &file_path);

            // First edit — snapshot should be created
            let original = std::fs::read(&file_path).unwrap();
            let created = store.ensure_snapshot(&file_path, &original).unwrap();
            assert!(created);

            let call1 = EditToolCall {
                path: file_path.clone(),
                old_string: "a = 1".to_string(),
                new_string: "a = 10".to_string(),
                replace_all: false,
            };
            let (_, new_bytes) = call1.execute(&file_path, &tracker);
            simulate_tracker_update(&mut tracker, &file_path, &new_bytes.unwrap());

            // Second edit — snapshot should NOT be recreated
            let content_before_second = std::fs::read(&file_path).unwrap();
            let created = store.ensure_snapshot(&file_path, &content_before_second).unwrap();
            assert!(!created); // idempotent — already snapshotted
        }

        #[rstest]
        fn permission_cache_grant_and_check() {
            let mut cache = EditPermissionCache::default();
            let path = std::path::PathBuf::from("/Users/me/.config/atuin/config.toml");

            // Initially no grant
            assert!(!cache.has_valid_grant(&path));

            // Grant permission
            cache.grant(path.clone());
            assert!(cache.has_valid_grant(&path));

            // Different file has no grant
            assert!(!cache.has_valid_grant(std::path::Path::new("/other/file.toml")));

            // Roundtrip through JSON (simulates session persistence)
            let json = cache.to_json().unwrap();
            let restored = EditPermissionCache::from_json(&json).unwrap();
            assert!(restored.has_valid_grant(&path));
        }
    }

    #[rstest]
    fn push_tail_keeps_the_end_within_twice_the_limit() {
        let mut buf = Vec::new();
        let chunk = vec![b'a'; RAW_OUTPUT_LIMIT_BYTES / 4];
        for _ in 0..12 {
            push_tail(&mut buf, &chunk);
            assert!(buf.len() <= 2 * RAW_OUTPUT_LIMIT_BYTES, "{}", buf.len());
        }
        push_tail(&mut buf, b"the end");
        assert!(buf.len() >= RAW_OUTPUT_LIMIT_BYTES);
        assert!(buf.ends_with(b"the end"));
    }

    /// Each command keeps running past its shell's death or its output's close; only stopping
    /// its process group, while still watching for the interrupt, ends it promptly.
    #[cfg(unix)]
    #[rstest]
    #[case::background_job_holds_the_output("sleep 30 & touch {ready}; wait")]
    #[case::output_closed_early("exec >/dev/null 2>&1; touch {ready}; sleep 30")]
    #[tokio::test]
    async fn an_interrupt_stops_the_whole_command(#[case] command: &str) {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let command = command.replace("{ready}", &ready.to_string_lossy());
        let call = ShellToolCall::try_from(&serde_json::json!({ "command": command })).unwrap();
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move { while output_rx.recv().await.is_some() {} });
        let (interrupt_tx, interrupt_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let _ = interrupt_tx.send(());
        });

        let run = execute_shell_command_streaming(&call, output_tx, interrupt_rx);
        let outcome = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .expect("the interrupt stops the command");
        assert!(
            matches!(outcome, ToolOutcome::Structured {
                interrupted: true,
                ..
            }),
            "{outcome:?}"
        );
    }

    // ── write_file execution tests ──

    mod write {
        use super::*;

        #[fixture]
        fn tempdir() -> tempfile::TempDir {
            tempfile::tempdir().unwrap()
        }

        #[rstest]
        fn creates_new_file(tempdir: tempfile::TempDir) {
            let path = tempdir.path().join("new_file.txt");

            let call = WriteToolCall {
                path: path.clone(),
                content: "hello\nworld\n".to_string(),
                overwrite: false,
            };
            let (outcome, new_bytes) = call.execute(&path, &Default::default());

            assert!(matches!(outcome, ToolOutcome::Success(ref s) if s.contains("Created")));
            assert!(new_bytes.is_some());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\nworld\n");
        }

        #[rstest]
        #[case::rejects_without_overwrite(false, "new content", true, "original")]
        #[case::overwrites_with_flag(true, "replaced content\n", false, "replaced content\n")]
        fn write_over_existing(
            tempdir: tempfile::TempDir,
            #[case] overwrite: bool,
            #[case] new_content: &str,
            #[case] expect_error: bool,
            #[case] expected_final: &str,
        ) {
            let path = tempdir.path().join("existing.txt");
            std::fs::write(&path, "original").unwrap();
            let mut tracker = crate::file_tracker::FileReadTracker::default();
            let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
            tracker.record_read(path.clone(), b"original", mtime);

            let call = WriteToolCall {
                path: path.clone(),
                content: new_content.to_string(),
                overwrite,
            };
            let (outcome, new_bytes) = call.execute(&path, &tracker);

            if expect_error {
                assert!(new_bytes.is_none());
                match outcome {
                    ToolOutcome::Error(msg) => {
                        assert!(msg.contains("already exists"), "got: {msg}");
                        assert!(msg.contains("overwrite"), "got: {msg}");
                    }
                    _ => panic!("expected error"),
                }
            } else {
                assert!(matches!(outcome, ToolOutcome::Success(_)));
                assert!(new_bytes.is_some());
            }
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected_final);
        }

        #[rstest]
        fn creates_parent_directories(tempdir: tempfile::TempDir) {
            let path = tempdir.path().join("sub").join("dir").join("file.txt");

            let call = WriteToolCall {
                path: path.clone(),
                content: "nested\n".to_string(),
                overwrite: false,
            };
            let (outcome, _) = call.execute(&path, &Default::default());

            assert!(matches!(outcome, ToolOutcome::Success(_)));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "nested\n");
        }

        /// Overwriting what the model hasn't seen would lose it: a file never read, or changed
        /// since (here recorded as read long ago with other content).
        #[rstest]
        #[case::never_read(None, "not been read yet")]
        #[case::changed_since_read(Some(b"what the model saw".as_slice()), "modified since read")]
        fn overwriting_needs_a_fresh_read(
            tempdir: tempfile::TempDir,
            #[case] read: Option<&[u8]>,
            #[case] expected_error: &str,
        ) {
            let path = tempdir.path().join("existing.txt");
            std::fs::write(&path, "the user's latest").unwrap();
            let mut tracker = crate::file_tracker::FileReadTracker::default();
            if let Some(content) = read {
                tracker.record_read(path.clone(), content, std::time::SystemTime::UNIX_EPOCH);
            }

            let call = WriteToolCall {
                path: path.clone(),
                content: "the model's version".to_string(),
                overwrite: true,
            };
            let (outcome, new_bytes) = call.execute(&path, &tracker);

            assert!(new_bytes.is_none());
            assert!(
                matches!(outcome, ToolOutcome::Error(ref msg) if msg.contains(expected_error)),
                "{outcome:?}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "the user's latest");
        }

        #[rstest]
        fn error_path_is_directory(tempdir: tempfile::TempDir) {
            let path = tempdir.path().to_path_buf();

            let call = WriteToolCall {
                path: path.clone(),
                content: "content".to_string(),
                overwrite: false,
            };
            let (outcome, new_bytes) = call.execute(&path, &Default::default());

            assert!(new_bytes.is_none());
            assert!(matches!(outcome, ToolOutcome::Error(ref msg) if msg.contains("directory")));
        }
    }
}
