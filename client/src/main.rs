//! `atuin-ai`: the Atuin AI shell agent, for any Atuin AI server.

use std::process::ExitCode;

use clap::Parser as _;
use eyre::{Context as _, Result};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

use crate::commands::{Cli, Command, InlineArgs};
use crate::settings::Settings;

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
mod process;
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
    match Cli::parse().command {
        // `init` runs at every shell startup, so it reads no settings and logs nothing: a broken
        // ai.toml is reported when `?` is pressed, not by every new shell.
        Command::Init { shell } => {
            println!("{}", shell::init(shell));
            Ok(())
        }
        Command::Inline(args) => inline(args),
    }
}

fn inline(args: InlineArgs) -> Result<()> {
    let _log_guard = init_logging(args.verbose);
    let settings = Settings::load()?;

    // The resume lookup, persistence and tool workers run alongside the TUI.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    // `block_on` rather than `spawn`: eye_declare holds a `StdoutLock` across await points, so
    // the TUI's future is not `Send`.
    runtime.block_on(commands::inline::run(args, settings))
}

/// The log file in Atuin's logs directory, rotated daily. Not Atuin's `ai.log`: pruning old logs
/// goes by file name, and would delete Atuin's.
const LOG_FILE: &str = "atuin-ai.log";
/// How many daily log files to keep.
const LOG_RETENTION_DAYS: usize = 4;

/// Log at `info` to [`LOG_FILE`], and to stderr when `stderr` is set. `ATUIN_LOG` overrides the
/// level with an `EnvFilter` directive, as it does for Atuin.
///
/// A log file that can't be opened turns file logging off rather than failing the command: the
/// shell widget would otherwise eat the user's prompt for a sidecar.
fn init_logging(stderr: bool) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = || EnvFilter::try_from_env("ATUIN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let (file_layer, guard, file_error) = match rolling_file(&dirs::logs_dir()) {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            (Some(layer), Some(guard), None)
        }
        Err(err) => (None, None, Some(err)),
    };

    let stderr_layer = stderr.then(|| {
        tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_filter(filter())
    });

    tracing_subscriber::registry().with(file_layer).with(stderr_layer).init();
    // Logged rather than printed: the shell widget reads the agent's answer from stderr.
    if let Some(err) = file_error {
        tracing::warn!(?err, "failed to open the log file; file logging disabled");
    }
    guard
}

fn rolling_file(dir: &std::path::Path) -> Result<tracing_appender::rolling::RollingFileAppender> {
    // Pruning old logs reads the directory, and fails loudly when it doesn't exist yet.
    fs_err::create_dir_all(dir)?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(LOG_FILE)
        .max_log_files(LOG_RETENTION_DAYS)
        .build(dir)
        .with_context(|| format!("failed to open {LOG_FILE} in {}", dir.display()))?;
    restrict_log_files(dir, LOG_FILE);
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
