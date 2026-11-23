//! The agent's settings: the `[ai]` table of Atuin's `config.toml`, plus the few Atuin settings it
//! shares (`data_dir`, `db_path`, `local_timeout`, `[logs]`).
//!
//! Atuin itself ignores `[ai]` since it dropped the agent, so the keys users already have keep
//! working here unchanged. Like Atuin, an `ATUIN_<KEY>` variable overrides a top-level key and
//! `ATUIN_AI__<KEY>` a key in `[ai]`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use config::{Config, Environment, File, FileFormat};
use eyre::{Context as _, Result};
use secrecy::SecretString;
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
    pub data_dir: PathBuf,
    /// Atuin's history database, which the `atuin_history` tool reads.
    pub db_path: PathBuf,
    /// How long to wait on a locked SQLite database.
    pub local_timeout: Duration,
    pub logs: Logs,
    pub ai: Ai,
}

#[derive(Clone, Debug)]
pub struct Ai {
    /// `Some(false)` turns the agent off: `init` binds nothing and `inline` exits at once.
    pub enabled: Option<bool>,
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

#[derive(Clone, Debug)]
pub struct Logs {
    /// The log file, or `None` when file logging is off.
    pub file: Option<PathBuf>,
    pub level: LogLevel,
    /// How many daily log files to keep.
    pub retention_days: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, derive_more::Display)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    #[display("trace")]
    Trace,
    #[display("debug")]
    Debug,
    #[default]
    #[display("info")]
    Info,
    #[display("warn")]
    Warn,
    #[display("error")]
    Error,
}

/// The settings as written, before paths are resolved and defaults filled in.
#[derive(Default, Deserialize)]
struct Raw {
    data_dir: Option<String>,
    db_path: Option<String>,
    #[serde(default, deserialize_with = "seconds")]
    local_timeout: Option<Duration>,
    #[serde(default)]
    logs: RawLogs,
    #[serde(default)]
    ai: RawAi,
}

#[derive(Default, Deserialize)]
struct RawAi {
    enabled: Option<bool>,
    endpoint: Option<Url>,
    api_token: Option<SecretString>,
    db_path: Option<String>,
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
}

/// Atuin's `[logs]` table. Its `[logs.ai]` is left to Atuin, whose Threads still log to `ai.log`.
#[derive(Default, Deserialize)]
struct RawLogs {
    enabled: Option<bool>,
    dir: Option<String>,
    level: Option<LogLevel>,
    #[serde(default, deserialize_with = "days")]
    retention: Option<Duration>,
}

/// Atuin's defaults for the settings it shares with the agent.
const DEFAULT_LOCAL_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_SESSION_CONTINUE: Duration = Duration::from_secs(60 * 60);
const DEFAULT_LOG_RETENTION_DAYS: usize = 4;
/// Not Atuin's `ai.log`: log pruning goes by file name, and would delete Atuin's.
const LOG_FILE: &str = "atuin-ai.log";

/// The path of Atuin's `config.toml`, which holds the agent's settings.
#[must_use]
pub fn config_file() -> PathBuf {
    dirs::config_path("config.toml")
}

impl Settings {
    /// Load the settings from Atuin's `config.toml` and the environment.
    pub fn load() -> Result<Self> {
        let file = config_file();
        let raw: Raw = Config::builder()
            .add_source(File::from(file.as_path()).format(FileFormat::Toml).required(false))
            .add_source(Environment::with_prefix("atuin").prefix_separator("_").separator("__"))
            .build()
            .and_then(Config::try_deserialize)
            .with_context(|| format!("failed to load settings from {}", file.display()))?;
        let config_dir = file.parent().unwrap_or(Path::new("/"));
        Self::resolve(raw, config_dir)
    }

    fn resolve(raw: Raw, config_dir: &Path) -> Result<Self> {
        let path = |raw: Option<String>, default: &dyn Fn() -> PathBuf| match raw {
            Some(raw) if !raw.is_empty() => resolve_path(&raw, config_dir),
            _ => Ok(default()),
        };

        let data_dir = path(raw.data_dir, &dirs::unconfigured_data_dir)?;
        let db_path = path(raw.db_path, &|| data_dir.join("history.db"))?;
        let ai_db_path = path(raw.ai.db_path, &|| data_dir.join("ai_sessions.db"))?;
        let logs_dir = path(raw.logs.dir, &dirs::logs_dir)?;

        let logs = Logs {
            file: raw.logs.enabled.unwrap_or(true).then(|| logs_dir.join(LOG_FILE)),
            level: raw.logs.level.unwrap_or_default(),
            retention_days: raw.logs.retention.map_or(DEFAULT_LOG_RETENTION_DAYS, |retention| {
                usize::try_from(retention.as_secs() / 86_400).unwrap_or(usize::MAX)
            }),
        };

        let ai = Ai {
            enabled: raw.ai.enabled,
            endpoint: raw.ai.endpoint,
            api_token: raw.ai.api_token,
            db_path: ai_db_path,
            session_continue: ResumeWindow::new(
                raw.ai.session_continue_minutes.unwrap_or(DEFAULT_SESSION_CONTINUE),
            ),
            model: raw.ai.model,
            yolo: raw.ai.yolo,
            send_cwd: raw.ai.send_cwd,
            opening: raw.ai.opening,
            capabilities: raw.ai.capabilities,
            tips: raw.ai.tips,
        };

        Ok(Self {
            data_dir,
            db_path,
            local_timeout: raw.local_timeout.unwrap_or(DEFAULT_LOCAL_TIMEOUT),
            logs,
            ai,
        })
    }

    /// Settings with every default, rooted in a directory that doesn't exist.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        let raw = Raw {
            data_dir: Some("/nonexistent/atuin-ai-test".to_owned()),
            logs: RawLogs {
                dir: Some("/nonexistent/atuin-ai-test/logs".to_owned()),
                ..RawLogs::default()
            },
            ..Raw::default()
        };
        Self::resolve(raw, Path::new("/")).expect("the default settings resolve")
    }
}

/// Expand `~` and `$VAR` in a configured path, then resolve it next to `config.toml` if relative.
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

fn seconds<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
    duration(de, Duration::from_secs(1))
}

fn minutes<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
    duration(de, Duration::from_secs(60))
}

fn days<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
    duration(de, Duration::from_secs(86_400))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// Load `toml` as `config.toml` in `/home/me/.atuin`. Every default that depends on the
    /// environment is pinned first, so a test sees the same paths on every machine.
    fn load(toml: &str) -> Settings {
        let raw: Raw = Config::builder()
            .set_default("data_dir", "/home/me/.atuin/data")
            .and_then(|builder| builder.set_default("logs.dir", "/home/me/.atuin/logs"))
            .expect("the defaults are valid keys")
            .add_source(File::from_str(toml, FileFormat::Toml))
            .build()
            .and_then(Config::try_deserialize)
            .expect("the test config parses");
        Settings::resolve(raw, Path::new("/home/me/.atuin")).expect("the test config resolves")
    }

    #[rstest]
    fn reads_the_ai_table_atuin_users_already_have() {
        let settings = load(
            "[ai]\nendpoint = \"http://localhost:8080\"\nmodel = \"llama\"\nyolo = \
             true\n[ai.opening]\nsend_cwd = true\n",
        );
        assert_eq!(settings.ai.endpoint.unwrap().as_str(), "http://localhost:8080/");
        assert_eq!(settings.ai.model.as_deref(), Some("llama"));
        assert!(settings.ai.yolo);
        assert_eq!(settings.ai.opening.send_cwd, Some(true));
    }

    #[rstest]
    fn ignores_the_keys_only_atuin_reads() {
        let settings = load("enter_accept = true\n[sync]\nrecords = true\n[ai]\ntips = false\n");
        assert_eq!(settings.ai.tips, Some(false));
    }

    #[rstest]
    fn relative_paths_resolve_next_to_config_toml() {
        let settings = load("data_dir = \"data\"\n[ai]\ndb_path = \"ai.db\"\n");
        assert_eq!(settings.data_dir, Path::new("/home/me/.atuin/data"));
        assert_eq!(settings.db_path, Path::new("/home/me/.atuin/data/history.db"));
        assert_eq!(settings.ai.db_path, Path::new("/home/me/.atuin/ai.db"));
    }

    #[rstest]
    #[case::default("", Some(Duration::from_secs(3600)))]
    #[case::bare_minutes("session_continue_minutes = 5", Some(Duration::from_secs(300)))]
    #[case::text("session_continue_minutes = \"90s\"", Some(Duration::from_secs(90)))]
    #[case::zero_disables("session_continue_minutes = 0", None)]
    fn session_continue(#[case] line: &str, #[case] expected: Option<Duration>) {
        let settings = load(&format!("[ai]\n{line}\n"));
        assert_eq!(settings.ai.session_continue.map(ResumeWindow::get), expected);
    }

    #[rstest]
    #[case::default("", Some("/home/me/.atuin/logs/atuin-ai.log"))]
    #[case::own_dir("[logs]\ndir = \"/var/log\"", Some("/var/log/atuin-ai.log"))]
    #[case::off("[logs]\nenabled = false", None)]
    #[case::atuin_s_own_ai_log_is_left_alone(
        "[logs.ai]\nfile = \"other.log\"",
        Some("/home/me/.atuin/logs/atuin-ai.log")
    )]
    fn log_file(#[case] toml: &str, #[case] expected: Option<&str>) {
        assert_eq!(load(toml).logs.file.as_deref(), expected.map(Path::new));
    }
}
