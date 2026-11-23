use std::path::PathBuf;
use std::sync::Arc;

use secrecy::SecretString;

use crate::distro::detect_linux_distribution;
use crate::history::{History, HistoryDb};
use crate::settings::AiCapabilities;
use crate::tools::descriptor;

/// Session-scoped context for the AI chat session.
/// Holds the API configuration and client settings needed by the event loop and stream task.
#[derive(Clone, Debug)]
pub struct AppContext {
    pub endpoint: reqwest::Url,
    /// Bearer token for `endpoint`. `None` means unauthenticated -- no
    /// Authorization header is sent (the server may not require auth).
    pub token: Option<SecretString>,
    pub send_cwd: bool,
    pub last_command: Option<History>,
    /// Atuin's history, or `None` when it couldn't be opened, which leaves
    /// the `atuin_history` tool out.
    pub history_db: Option<Arc<HistoryDb>>,
    /// Repository root of the current working directory, if inside one. A git worktree
    /// resolves to the main repo root; a jj workspace keeps its own root.
    pub vcs_root: Option<PathBuf>,
    pub capabilities: AiCapabilities,
    pub yolo: bool,
}

/// The `client_v1_*` capability strings this client sends with every AI
/// request to the server (`ai.endpoint`), which runs the model and sends back
/// tool calls. The server reads the list to decide which tools it may ask this
/// client to run: a tool whose capability string is missing here is never
/// offered to the model. Requests are built in more than one place, but they
/// all get their capabilities from this function — separately maintained
/// lists could drift apart, and the model would then see different tools
/// depending on how the user launched the AI.
///
/// `ATUIN_AI__ADDITIONAL_CAPS`, a comma-separated list, appends more: for
/// trying a server's new tool before this client knows it.
pub fn capability_strings(capabilities: &AiCapabilities, history_available: bool) -> Vec<String> {
    // Each tool's capability string lives on its ToolDescriptor. Reuse it
    // rather than retyping the string: the fsm later accepts or rejects the
    // server's tool calls by comparing against those same descriptors, so a
    // mismatched literal here would silently disable a tool.
    let cap = |d: &descriptor::ToolDescriptor| {
        d.capability.expect("client-side tools declare a capability").to_string()
    };
    let mut caps = vec!["client_invocations".to_string(), cap(descriptor::LOAD_SKILL)];
    if history_available && capabilities.enable_history_search.unwrap_or(true) {
        caps.push(cap(descriptor::ATUIN_HISTORY));
        caps.push(descriptor::ATUIN_HISTORY_V2_CAPABILITY.to_string());
    }
    if capabilities.enable_file_tools.unwrap_or(true) {
        caps.push(cap(descriptor::READ));
        caps.push(cap(descriptor::EDIT));
        caps.push(cap(descriptor::WRITE));
    }
    if capabilities.enable_command_execution.unwrap_or(true) {
        caps.push(cap(descriptor::SHELL));
    }
    if let Ok(extra) = std::env::var("ATUIN_AI__ADDITIONAL_CAPS") {
        caps.extend(extra.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    }
    caps
}

impl AppContext {
    pub(crate) fn capabilities_as_strings(&self) -> Vec<String> {
        capability_strings(&self.capabilities, self.history_db.is_some())
    }
}

/// Machine identity — computed once per session.
#[derive(Clone, Debug)]
pub struct ClientContext {
    pub os: String,
    pub shell: Option<String>,
    pub distro: Option<String>,
}

impl ClientContext {
    pub(crate) fn detect() -> Self {
        let os = detect_os();
        let shell = Some(detect_shell());
        let distro = if os == "linux" {
            Some(detect_linux_distribution())
        } else {
            None
        };
        Self { os, shell, distro }
    }

    /// Serialize to the JSON format the API expects for the "context" field.
    /// The `pwd` field is always dynamic (current working directory), so it's
    /// computed fresh on each call if `send_cwd` is true.
    pub(crate) fn to_json(
        &self,
        send_cwd: bool,
        last_command: Option<&History>,
    ) -> serde_json::Value {
        let mut ctx = serde_json::json!({
            "os": self.os,
            "shell": self.shell,
            "pwd": if send_cwd {
                std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned())
            } else {
                None
            },
        });

        if let Some(history) = last_command {
            ctx["last_command"] = serde_json::json!(crate::history_format::format_last_command(
                history,
                crate::history_format::local_offset(),
            ));
        }

        if let Some(ref distro) = self.distro {
            ctx["distro"] = serde_json::json!(distro);
        }

        ctx
    }
}

/// The shell the agent runs under: `$ATUIN_SHELL`, which the widgets set as
/// Atuin's do, else the name of `$SHELL`, else `unknown`.
fn detect_shell() -> String {
    let from_env = |var| {
        let value = std::env::var(var).ok()?;
        let name = value.trim().rsplit('/').next()?.trim_start_matches('-').to_lowercase();
        (!name.is_empty()).then_some(name)
    };
    from_env("ATUIN_SHELL").or_else(|| from_env("SHELL")).unwrap_or_else(|| "unknown".to_owned())
}

fn detect_os() -> String {
    match std::env::consts::OS {
        "macos" => "macos".to_string(),
        "linux" => "linux".to_string(),
        "windows" => "windows".to_string(),
        other => format!("Other: {other}"),
    }
}
