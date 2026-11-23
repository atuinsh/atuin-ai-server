//! The agent's settings, from `ai.toml` in Atuin's config directory (`~/.atuin/ai.toml` by
//! default). An `ATUIN_AI__<KEY>` variable overrides a key, with `__` between nested keys:
//! `ATUIN_AI__CAPABILITIES__ENABLE_FILE_TOOLS=false`.
//!
//! The keys are those of the `[ai]` table in Atuin's `config.toml` from before Atuin dropped the
//! agent, moved to the top level, plus where Atuin's history is. The agent keeps its own files in
//! Atuin's data directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

use config::{Config, Environment, File, FileFormat};
use eyre::{Context as _, Result};
use secrecy::SecretString;
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};
use url::Url;

use crate::dirs;

/// How long a past conversation stays resumable: never zero, since `0` turns resuming off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResumeWindow(Duration);

impl ResumeWindow {
    /// `None` for a zero window, which disables resuming.
    #[must_use]
    pub fn new(window: Duration) -> Option<Self> {
        (!window.is_zero()).then_some(Self(window))
    }

    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// Atuin's data directory, which holds the agent's conversations and file backups.
    pub data_dir: PathBuf,
    /// Atuin's history database, which the `atuin_history` tool reads.
    pub history_db_path: PathBuf,
    pub ai: Ai,
}

#[derive(Clone, Debug)]
pub struct Ai {
    /// The Atuin AI server to chat with, e.g. `http://localhost:8080`.
    pub endpoint: Option<Url>,
    /// Bearer token for `endpoint`. Empty means unauthenticated.
    pub api_token: Option<SecretString>,
    /// The agent's own conversation database.
    pub db_path: PathBuf,
    /// How long a conversation in the same directory is picked up again, if at all.
    pub session_continue: Option<ResumeWindow>,
    /// The server's model alias to request; the server's default when unset.
    pub model: Option<String>,
    /// Skip every permission prompt.
    pub yolo: bool,
    /// Deprecated spelling of `opening.send_cwd`.
    pub send_cwd: Option<bool>,
    pub opening: AiOpening,
    pub capabilities: AiCapabilities,
    /// Whether the TUI shows feature tips. `None` means it does.
    pub tips: Option<bool>,
}

/// Tool capability flags. `None` means enabled, asking permission per call.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AiCapabilities {
    pub enable_history_search: Option<bool>,
    pub enable_file_tools: Option<bool>,
    pub enable_command_execution: Option<bool>,
}

/// What the opening request tells the server about where the user is.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AiOpening {
    pub send_cwd: Option<bool>,
    pub send_last_command: Option<bool>,
}

/// `ai.toml` as written, before paths are resolved and defaults filled in.
#[derive(Default, Deserialize)]
struct Raw {
    endpoint: Option<Url>,
    api_token: Option<SecretString>,
    #[serde(default, deserialize_with = "minutes")]
    session_continue_minutes: Option<Duration>,
    model: Option<String>,
    #[serde(default)]
    yolo: bool,
    send_cwd: Option<bool>,
    #[serde(default)]
    opening: AiOpening,
    #[serde(default)]
    capabilities: AiCapabilities,
    tips: Option<bool>,
    history_db_path: Option<String>,
    /// An `[ai]` table copied whole from `config.toml`, header included. Rejected: its keys go
    /// at the top level here.
    ai: Option<IgnoredAny>,
}

const DEFAULT_SESSION_CONTINUE: Duration = Duration::from_secs(60 * 60);

/// The path of `ai.toml`, which holds the agent's settings.
#[must_use]
pub fn config_file() -> PathBuf {
    dirs::config_path("ai.toml")
}

impl Settings {
    /// Load the settings from `ai.toml` and the environment.
    pub fn load() -> Result<Self> {
        let file = config_file();
        let raw: Raw = Config::builder()
            .add_source(File::from(file.as_path()).format(FileFormat::Toml).required(false))
            .add_source(Environment::with_prefix("atuin_ai").prefix_separator("__").separator("__"))
            .build()
            .and_then(Config::try_deserialize)
            .with_context(|| format!("failed to load settings from {}", file.display()))?;
        Self::resolve(raw, &file, dirs::unconfigured_data_dir())
    }

    /// Fill in `raw`'s defaults, resolving relative paths next to `file`. `data_dir` is Atuin's
    /// data directory.
    fn resolve(raw: Raw, file: &Path, data_dir: PathBuf) -> Result<Self> {
        if raw.ai.is_some() {
            eyre::bail!(
                "{} has an `[ai]` table: write its keys at the top level instead, without the \
                 `[ai]` header, and `[ai.opening]` and `[ai.capabilities]` as `[opening]` and \
                 `[capabilities]`",
                file.display()
            );
        }

        let config_dir = file.parent().unwrap_or(Path::new("/"));
        let history_db_path = match raw.history_db_path {
            Some(raw) if !raw.is_empty() => resolve_path(&raw, config_dir)?,
            _ => data_dir.join("history.db"),
        };

        let ai = Ai {
            endpoint: raw.endpoint,
            api_token: raw.api_token,
            db_path: data_dir.join("ai_sessions.db"),
            session_continue: ResumeWindow::new(
                raw.session_continue_minutes.unwrap_or(DEFAULT_SESSION_CONTINUE),
            ),
            model: raw.model,
            yolo: raw.yolo,
            send_cwd: raw.send_cwd,
            opening: raw.opening,
            capabilities: raw.capabilities,
            tips: raw.tips,
        };

        Ok(Self {
            data_dir,
            history_db_path,
            ai,
        })
    }

    /// Settings with every default, rooted in a directory that doesn't exist.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        let raw = Raw::default();
        Self::resolve(raw, Path::new("/ai.toml"), PathBuf::from("/nonexistent/atuin-ai-test"))
            .expect("the default settings resolve")
    }
}

/// Expand `~` and `$VAR` in a configured path, then resolve it next to `ai.toml` if relative.
fn resolve_path(raw: &str, config_dir: &Path) -> Result<PathBuf> {
    let expanded =
        shellexpand::full(raw).with_context(|| format!("failed to expand the path {raw:?}"))?;
    let path = PathBuf::from(expanded.as_ref());
    Ok(if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    })
}

/// A duration written as a bare number of `unit`s, or as a string such as `90s` or `2h`.
fn duration<'de, D: Deserializer<'de>>(
    de: D,
    unit: Duration,
) -> Result<Option<Duration>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Written {
        Number(f64),
        Text(String),
    }

    let invalid = |what: &dyn std::fmt::Display| {
        serde::de::Error::custom(format_args!("invalid duration {what}"))
    };
    let units =
        |n: f64| Duration::try_from_secs_f64(n * unit.as_secs_f64()).map_err(|_| invalid(&n));

    match Option::<Written>::deserialize(de)? {
        None => Ok(None),
        Some(Written::Number(n)) => units(n).map(Some),
        Some(Written::Text(text)) => match text.trim().parse::<f64>() {
            Ok(n) => units(n).map(Some),
            Err(_) => humantime::parse_duration(&text).map(Some).map_err(|_| invalid(&text)),
        },
    }
}

fn minutes<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
    duration(de, Duration::from_secs(60))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// Load `toml` as `/home/me/.atuin/ai.toml`, with Atuin's data directory in `/home/me/.atuin/data`.
    fn try_load(toml: &str) -> Result<Settings> {
        let raw: Raw = Config::builder()
            .add_source(File::from_str(toml, FileFormat::Toml))
            .build()
            .and_then(Config::try_deserialize)
            .expect("the test config parses");
        Settings::resolve(
            raw,
            Path::new("/home/me/.atuin/ai.toml"),
            PathBuf::from("/home/me/.atuin/data"),
        )
    }

    fn load(toml: &str) -> Settings {
        try_load(toml).expect("the test config resolves")
    }

    const EXAMPLE: &str = include_str!("../ai.example.toml");

    /// `EXAMPLE` with every commented-out `key = value` line switched on, however many `#` it has.
    fn example_uncommented() -> String {
        let is_key = |key: &str| key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
        EXAMPLE
            .lines()
            .map(|line| match line.strip_prefix('#') {
                Some(rest) => {
                    let rest = rest.trim_start_matches('#').trim_start();
                    let is_setting = rest.split_once(" = ").is_some_and(|(key, _)| is_key(key));
                    if is_setting {
                        rest
                    } else {
                        line
                    }
                }
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The effective value of each setting whose default the example states, paths aside.
    fn stated_defaults(settings: &Settings) -> impl PartialEq + std::fmt::Debug {
        let ai = &settings.ai;
        (
            ai.session_continue,
            ai.yolo,
            ai.tips.unwrap_or(true),
            ai.capabilities.enable_history_search.unwrap_or(true),
            ai.capabilities.enable_file_tools.unwrap_or(true),
            ai.capabilities.enable_command_execution.unwrap_or(true),
            ai.opening.send_cwd.or(ai.send_cwd).unwrap_or(false),
            ai.opening.send_last_command.unwrap_or(false),
        )
    }

    #[rstest]
    fn the_example_states_the_real_defaults() {
        let example = load(EXAMPLE);
        assert_eq!(example.ai.endpoint.as_ref().map(Url::as_str), Some("http://localhost:8080/"));

        let uncommented = load(&example_uncommented());
        assert_eq!(uncommented.ai.model.as_deref(), Some("llama"), "the keys were switched on");
        assert_eq!(stated_defaults(&uncommented), stated_defaults(&example));
    }

    #[rstest]
    fn reads_the_keys_of_atuin_s_old_ai_table_at_the_top_level() {
        let settings = load(
            "endpoint = \"http://localhost:8080\"\nmodel = \"llama\"\nyolo = \
             true\n[opening]\nsend_cwd = true\n[capabilities]\nenable_file_tools = false\n",
        );
        assert_eq!(settings.ai.endpoint.unwrap().as_str(), "http://localhost:8080/");
        assert_eq!(settings.ai.model.as_deref(), Some("llama"));
        assert!(settings.ai.yolo);
        assert_eq!(settings.ai.opening.send_cwd, Some(true));
        assert_eq!(settings.ai.capabilities.enable_file_tools, Some(false));
    }

    #[rstest]
    #[case::whole_table("[ai]\nendpoint = \"http://localhost:8080\"")]
    #[case::subtable_only("[ai.opening]\nsend_cwd = true")]
    fn rejects_an_ai_table_copied_with_its_header(#[case] toml: &str) {
        let err = try_load(toml).expect_err("an [ai] table is rejected");
        assert!(err.to_string().contains("without the `[ai]` header"), "{err}");
    }

    #[rstest]
    #[case::default("", "/home/me/.atuin/data/history.db")]
    #[case::absolute("history_db_path = \"/srv/atuin/history.db\"", "/srv/atuin/history.db")]
    #[case::relative_to_ai_toml("history_db_path = \"history.db\"", "/home/me/.atuin/history.db")]
    fn history_db_path(#[case] toml: &str, #[case] expected: &str) {
        assert_eq!(load(toml).history_db_path, Path::new(expected));
    }

    #[rstest]
    #[case::default("")]
    #[case::a_ported_db_path_is_ignored("db_path = \"ai.db\"")]
    fn conversations_live_in_atuin_s_data_dir(#[case] toml: &str) {
        assert_eq!(load(toml).ai.db_path, Path::new("/home/me/.atuin/data/ai_sessions.db"));
    }

    #[rstest]
    #[case::default("", Some(Duration::from_secs(3600)))]
    #[case::bare_minutes("session_continue_minutes = 5", Some(Duration::from_secs(300)))]
    #[case::text("session_continue_minutes = \"90s\"", Some(Duration::from_secs(90)))]
    #[case::zero_disables("session_continue_minutes = 0", None)]
    fn session_continue(#[case] toml: &str, #[case] expected: Option<Duration>) {
        assert_eq!(load(toml).ai.session_continue.map(ResumeWindow::get), expected);
    }
}
