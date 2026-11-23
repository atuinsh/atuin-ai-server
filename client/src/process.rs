//! Commands run for the model or a skill go in a process group of their own, so stopping one
//! stops everything it started, background jobs included.

use tokio::process::{Child, Command};

/// Run `cmd` in a new process group, which [`kill_tree`] and [`kill_group`] stop as a whole.
pub fn own_group(cmd: &mut Command) {
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Kill `child` and, on unix, the rest of its process group.
pub fn kill_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        kill_group(pid);
    }
    let _ = child.start_kill();
}

/// SIGKILL the process group led by `pid`, which [`own_group`] gave it. Off unix, a no-op: there,
/// only the process itself is killed.
pub fn kill_group(pid: u32) {
    #[cfg(unix)]
    if let Some(pid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) {
        // A group that is already gone has nothing left to stop.
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}
