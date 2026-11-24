use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use eyre::{Context as _, Result, eyre};
use secrecy::ExposeSecret as _;
use tracing::{debug, warn};

use crate::commands::InlineArgs;
use crate::context::{AppContext, ClientContext};
use crate::dirs;
use crate::fsm::AgentFsm;
use crate::history::HistoryDb;
use crate::session::{LocalSessionService, SessionManager, SessionService};
use crate::settings::{self, Settings};
use crate::tui::app::{AiApp, ExitOutcome, IoContext};
use crate::tui::state::ConversationEvent;

/// How long to wait on a locked SQLite database: Atuin's default `local_timeout`.
const DB_TIMEOUT: Duration = Duration::from_secs(2);

pub async fn run(args: InlineArgs, settings: Settings) -> Result<()> {
    let InlineArgs {
        api_endpoint,
        api_token,
        prompt: initial_command,
        hook: output_for_hook,
        ..
    } = args;
    let endpoint = match api_endpoint.as_deref() {
        Some(raw) => reqwest::Url::parse(raw).context("invalid --api-endpoint URL")?,
        None => settings.ai.endpoint.clone().ok_or_else(|| {
            no_endpoint(&settings::config_file(), dirs::try_home_dir().ok().as_deref())
        })?,
    };
    // An empty token means unauthenticated.
    let token = api_token
        .or_else(|| settings.ai.api_token.clone())
        .filter(|t| !t.expose_secret().is_empty());

    let history_db = open_history(&settings.history_db_path).await;

    // Support both the deprecated top-level send_cwd and [opening] send_cwd
    let send_cwd =
        settings.ai.opening.send_cwd.unwrap_or(false) || settings.ai.send_cwd.unwrap_or(false);

    let last_command = match &history_db {
        Some(db) if settings.ai.opening.send_last_command.unwrap_or(false) => {
            db.last().await.ok().flatten()
        }
        _ => None,
    };

    let vcs_root = std::env::current_dir().ok().and_then(|cwd| crate::vcs::root(&cwd));

    let ctx = AppContext {
        endpoint,
        token,
        send_cwd,
        last_command,
        history_db,
        vcs_root,
        capabilities: settings.ai.capabilities.clone(),
        yolo: settings.ai.yolo,
    };

    let action = run_inline_tui(ctx, initial_command, &Arc::new(settings)).await?;
    emit_shell_result(action, output_for_hook);

    Ok(())
}

/// The error for a missing `endpoint`. Usually `ai.toml` doesn't exist yet: on a fresh install, or
/// for a user whose settings are still in the `[ai]` table of Atuin's `config.toml`.
fn no_endpoint(file: &Path, home: Option<&Path>) -> eyre::Report {
    let shown = crate::path::display_from_home(file, home);
    if file.exists() {
        eyre!("{shown} sets no `endpoint`; add one, e.g. `endpoint = \"http://localhost:8080\"`")
    } else {
        eyre!("could not find {shown}. Coming from Atuin's config.toml? Move its [ai] table there.")
    }
}

/// Atuin's history database, or `None` when it can't be opened: the chat still works, without
/// the `atuin_history` tool or the last command as context.
async fn open_history(path: &Path) -> Option<Arc<HistoryDb>> {
    match HistoryDb::open(path, DB_TIMEOUT).await {
        Ok(db) => Some(Arc::new(db)),
        Err(err) => {
            warn!(?err, path = %path.display(), "failed to open Atuin's history; history search disabled");
            None
        }
    }
}

/// The directory where AI snapshots are stored.
///
/// Older versions of Atuin stored snapshots in the [legacy data dir] even when [`data_dir`] was set
/// in its config.toml. If the new snapshot dir doesn't exist, this function will check the legacy path
/// and return it if it exists, unless `ATUIN_HOME` is set.
///
/// [legacy data dir]: dirs::legacy_data_dir
/// [`data_dir`]: field@Settings::data_dir
fn snapshots_dir(settings: &Settings) -> PathBuf {
    fn join_snapshots(path: impl Into<PathBuf>) -> PathBuf {
        path.into().join("ai").join("snapshots")
    }

    let path = join_snapshots(&settings.data_dir);
    if !path.exists() && !dirs::atuin_home_is_set() {
        // Check the legacy path, but only if `ATUIN_HOME` isn't set.
        let legacy = join_snapshots(dirs::legacy_data_dir());
        if legacy.exists() {
            return legacy;
        }
    }
    path
}

async fn run_inline_tui(
    ctx: AppContext,
    initial_prompt: Option<String>,
    settings: &Arc<Settings>,
) -> Result<ExitOutcome> {
    let client_ctx = ClientContext::detect();

    // Open the session service and check for a resumable session
    let service = LocalSessionService::open(&settings.ai.db_path, DB_TIMEOUT)
        .await
        .context("failed to open AI session database")?;

    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned());
    let vcs_root_str = ctx.vcs_root.as_ref().map(|p| p.to_string_lossy().into_owned());

    let resumable = service
        .find_resumable(cwd.as_deref(), vcs_root_str.as_deref(), settings.ai.session_continue)
        .await?;

    // ─── Build FSM ───────────────────────────────────────────────
    let (session_mgr, mut fsm, file_tracker, edit_permissions) = if let Some(stored) = resumable {
        debug!(session_id = %stored.id, "resuming AI session");
        let (mgr, mut events, server_sid, last_event_ts, invocation_id) =
            SessionManager::resume(Box::new(service), &stored).await?;

        let has_api_content = events.iter().any(|e| e.is_api_content());

        if has_api_content {
            events.push(ConversationEvent::SystemContext {
                content: "[Note: The user has started a new invocation of Atuin AI. Prior \
                          messages from this session are from an earlier invocation.]"
                    .to_string(),
            });
            let view_start = events.len();
            let last_time = last_event_ts.and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));

            let ft = if let Ok(Some(json)) =
                mgr.get_branch_metadata(crate::file_tracker::METADATA_KEY).await
                && let Ok(tracker) = crate::file_tracker::FileReadTracker::from_json(&json)
            {
                tracker
            } else {
                Default::default()
            };

            let ep = if let Ok(Some(json)) =
                mgr.get_metadata(crate::edit_permissions::METADATA_KEY).await
                && let Ok(cache) = crate::edit_permissions::EditPermissionCache::from_json(&json)
            {
                cache
            } else {
                Default::default()
            };

            let caps = ctx.capabilities_as_strings();
            let fsm = AgentFsm::from_session(
                events,
                server_sid,
                caps,
                invocation_id,
                view_start,
                true,
                last_time,
            );
            (mgr, fsm, ft, ep)
        } else {
            debug!("resumable session has no API-visible content, starting fresh");
            let caps = ctx.capabilities_as_strings();
            let fsm = AgentFsm::new(caps, invocation_id);
            (mgr, fsm, Default::default(), Default::default())
        }
    } else {
        debug!("creating new AI session");
        let mgr =
            SessionManager::create_new(Box::new(service), cwd.as_deref(), vcs_root_str.as_deref());
        let invocation_id = uuid::Uuid::now_v7().to_string();
        let caps = ctx.capabilities_as_strings();
        let fsm = AgentFsm::new(caps, invocation_id);
        (mgr, fsm, Default::default(), Default::default())
    };

    // `model` is read once at startup, so /model in another running
    // session doesn't retarget this one mid-conversation.
    fsm.ctx.model =
        settings.ai.model.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);

    // ─── Snapshot store ─────────────────────────────────────────
    let snapshot_dir = snapshots_dir(settings).join(session_mgr.session_id());
    let snapshot_store = crate::snapshots::SnapshotStore::open(snapshot_dir).ok();

    // ─── Discover skills ───────────────────────────────────────
    let project_root = ctx.vcs_root.clone().or_else(|| std::env::current_dir().ok());
    let skill_registry = crate::skills::SkillRegistry::discover(project_root.as_deref()).await;

    // ─── Resume notice (frozen at startup) ──────────────────────
    let resume_notice = fsm.ctx.is_resumed.then(|| match fsm.ctx.last_event_time {
        Some(t) => {
            let human = chrono_humanize::HumanTime::from(t - chrono::Utc::now());
            format!(
                "  Continuing previous session (last active {human}) - type /new to start a new \
                 session"
            )
        }
        None => "  Continuing previous session - type /new to start a new session".to_string(),
    });

    // ─── Slash commands + skills ────────────────────────────────
    let mut slash_registry = crate::tui::slash::SlashCommandRegistry::default();
    let mut skill_names = std::collections::HashSet::new();
    for skill in skill_registry.all() {
        slash_registry
            .register(crate::tui::slash::SlashCommand::new(&skill.name, &skill.description));
        skill_names.insert(skill.name.clone());
    }

    // ─── IO context ─────────────────────────────────────────────
    // The persist worker owns the SessionManager and applies snapshots in
    // channel order.
    let (persist, persist_worker) = crate::tui::persist::spawn_persist_worker(session_mgr);
    let io = IoContext {
        app_ctx: ctx.clone(),
        client_ctx: client_ctx.clone(),
        persist,
        file_tracker,
        edit_permissions,
        snapshot_store,
        skill_registry,
    };

    println!();

    let app = AiApp::new(
        fsm,
        io,
        resume_notice,
        slash_registry,
        skill_names,
        initial_prompt,
        Arc::clone(settings),
    );
    let options =
        eye_declare::RunOptions::default().keyboard(eye_declare::KeyboardProtocol::Enhanced);
    let outcome =
        eye_declare::driver_tokio::run_with(app, options).await.context("failed running AI TUI")?;

    // The app (and with it the last persist sender) dropped when the run
    // loop returned; wait for the worker to drain its queue so the final
    // session snapshot is on disk before the process can exit.
    let _ = persist_worker.await;

    Ok(outcome)
}

fn emit_shell_result(outcome: ExitOutcome, output_for_hook: bool) {
    if output_for_hook {
        match outcome {
            ExitOutcome::Execute(output) => eprintln!("__atuin_ai_execute__:{output}"),
            ExitOutcome::Insert(output) => eprintln!("__atuin_ai_insert__:{output}"),
            ExitOutcome::Cancel => eprintln!("__atuin_ai_cancel__"),
        }
    } else {
        match outcome {
            ExitOutcome::Execute(output) | ExitOutcome::Insert(output) => {
                println!("{output}");
            }
            ExitOutcome::Cancel => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::no_file(
        false,
        "could not find ~/ai.toml. Coming from Atuin's config.toml? Move its [ai] table there."
    )]
    #[case::no_key(
        true,
        "~/ai.toml sets no `endpoint`; add one, e.g. `endpoint = \"http://localhost:8080\"`"
    )]
    fn no_endpoint_says_whether_ai_toml_exists(#[case] exists: bool, #[case] expected: &str) {
        let home = tempfile::tempdir().expect("a temp dir");
        let file = home.path().join("ai.toml");
        if exists {
            std::fs::write(&file, "").expect("the file is written");
        }
        assert_eq!(no_endpoint(&file, Some(home.path())).to_string(), expected);
    }
}
