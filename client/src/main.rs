//! `atuin-ai`: the Atuin AI shell agent, for any Atuin AI server.

use std::process::ExitCode;

use clap::Parser as _;
use eyre::{Context as _, Result};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

use crate::commands::{Cli, Command};
use crate::settings::{Logs, Settings};

mod ansi;
mod commands;
mod context;
mod context_window;
mod diff;
mod dirs;
mod distro;
mod edit_permissions;
mod ellipsis;
mod event_serde;
mod file_tracker;
mod fsm;
mod history;
mod history_format;
mod models;
mod path;
mod permissions;
mod session;
mod settings;
mod shell;
mod skills;
mod snapshots;
mod store;
mod stream;
mod tools;
mod tui;
mod vcs;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("atuin-ai: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let settings = Settings::load()?;
    // `init` runs at every shell startup, and has nothing worth a log file.
    let _log_guard = match &cli.command {
        Command::Inline { verbose, .. } => init_logging(&settings.logs, *verbose),
        Command::Init { .. } => None,
    };

    // The resume lookup, persistence and tool workers run alongside the TUI.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    // `block_on` rather than `spawn`: eye_declare holds a `StdoutLock` across await points, so
    // the TUI's future is not `Send`.
    runtime.block_on(commands::run(cli.command, settings))
}

/// Log to the configured file, and to stderr when `stderr` is set. `ATUIN_LOG` overrides the
/// configured level with an `EnvFilter` directive, as it does for Atuin.
///
/// A log file that can't be opened turns file logging off rather than failing the command: the
/// shell widget would otherwise eat the user's prompt for a sidecar.
fn init_logging(logs: &Logs, stderr: bool) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = || {
        EnvFilter::try_from_env("ATUIN_LOG")
            .unwrap_or_else(|_| EnvFilter::new(logs.level.to_string()))
    };

    let (file_layer, guard) = match logs.file.as_deref().map(|path| rolling_file(path, logs)) {
        Some(Ok(appender)) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            (Some(layer), Some(guard))
        }
        Some(Err(err)) => {
            eprintln!("atuin-ai: {err:#}; file logging disabled");
            (None, None)
        }
        None => (None, None),
    };

    let stderr_layer = stderr.then(|| {
        tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_filter(filter())
    });

    tracing_subscriber::registry().with(file_layer).with(stderr_layer).init();
    guard
}

fn rolling_file(
    path: &std::path::Path,
    logs: &Logs,
) -> Result<tracing_appender::rolling::RollingFileAppender> {
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("atuin-ai.log");
    // Pruning old logs reads the directory, and fails loudly when it doesn't exist yet.
    fs_err::create_dir_all(dir)?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(name)
        .max_log_files(logs.retention_days.max(1))
        .build(dir)
        .with_context(|| format!("failed to open the log file {}", path.display()))?;
    restrict_log_files(dir, name);
    Ok(appender)
}

/// Make the log files readable by the user alone, as Atuin's are: they hold prompts and commands.
///
/// Not via the umask, which files the agent writes for the user would inherit. A file the
/// appender rotates to mid-run keeps the umask's mode until the next run.
fn restrict_log_files(dir: &std::path::Path, prefix: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let private = std::fs::Permissions::from_mode(0o600);
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(|name| name.starts_with(prefix)) {
                let _ = std::fs::set_permissions(entry.path(), private.clone());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (dir, prefix);
}
