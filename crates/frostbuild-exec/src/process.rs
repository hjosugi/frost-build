//! Cancellation, process groups, and waiting on a child with a deadline.
//!
//! An action's process is not the only thing it starts. Everything here works
//! on the process *group* so that a compiler wrapper's children, a test
//! server, or a language server started by a rule dies with the build rather
//! than outliving it.

// Only the Windows `terminate_process_tree` below needs this, and a host
// build never compiles it — so it is gated rather than plain, or `cargo fix`
// deletes it as unused on Linux. `tests/test_cfg_imports.py` fails on the
// ungated form for exactly that reason.
#[cfg(windows)]
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;

use crate::options::DEFAULT_TEST_TIMEOUT;
use crate::{CANCELLED, RUNNING_PROCESS_GROUPS, SIGNAL_HANDLER};

pub fn install_signal_handler() -> Result<()> {
    if SIGNAL_HANDLER.get().is_some() {
        return Ok(());
    }
    ctrlc::set_handler(request_cancellation)?;
    let _ = SIGNAL_HANDLER.set(());
    Ok(())
}

/// Request the same cancellation performed by SIGINT. Interactive renderers
/// call this when raw terminal mode turns Ctrl-C into a key event.
pub fn request_cancellation() {
    CANCELLED.store(true, Ordering::SeqCst);
    if let Some(groups) = RUNNING_PROCESS_GROUPS.get() {
        for pid in groups.lock().unwrap().iter().copied() {
            terminate_process_tree(pid);
        }
    }
}

#[cfg(unix)]
pub(crate) fn terminate_process_tree(pid: u32) {
    // SAFETY: kill is async-process-safe; negative pid addresses the process
    // group created for this action immediately before it was spawned.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
}

#[cfg(windows)]
pub(crate) fn terminate_process_tree(pid: u32) {
    // taskkill is part of Windows and `/T` terminates descendants as well as
    // the direct compiler/test process. This keeps cancellation semantics
    // aligned with Unix process groups without requiring child handles to be
    // held behind the scheduler's shared lock.
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status();
}

/// Precedence for an action's limit, in one place because the order is the
/// contract: the target's own declaration is the most specific statement about
/// the work, the invocation speaks for the environment, and the default exists
/// only so that a hanging test cannot hold a CI job open by itself.
pub fn resolve_timeout(
    declared_secs: Option<u64>,
    requested: Option<Duration>,
    kind: frostbuild_core::graph::ActionKind,
) -> Option<Duration> {
    if let Some(seconds) = declared_secs {
        return Some(Duration::from_secs(seconds));
    }
    if let Some(limit) = requested {
        return Some(limit);
    }
    if kind == frostbuild_core::graph::ActionKind::Test {
        return Some(DEFAULT_TEST_TIMEOUT);
    }
    None
}

/// Wait for a child, optionally giving up.
///
/// The wait happens on a helper thread rather than by polling so that an
/// action with no limit — the default for build actions — keeps exactly the
/// wakeup behaviour it had before, and an action with one is not charged a
/// poll interval of latency either. On expiry the process *group* is
/// terminated, the same tree cancellation uses, and escalated if the group
/// ignores it; the output collected up to that point is still returned so the
/// failure report can show what the action managed to say.
pub(crate) fn wait_for_child(
    child: std::process::Child,
    pid: u32,
    limit: Option<Duration>,
) -> std::io::Result<(std::process::Output, bool)> {
    let Some(limit) = limit else {
        return child.wait_with_output().map(|output| (output, false));
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    match receiver.recv_timeout(limit) {
        Ok(output) => output.map(|output| (output, false)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            terminate_process_tree(pid);
            // A process that ignores the polite signal still has to stop: the
            // whole point of a limit is that it is not advisory.
            let output = match receiver.recv_timeout(TIMEOUT_KILL_GRACE) {
                Ok(output) => output,
                Err(_) => {
                    kill_process_tree(pid);
                    receiver.recv().map_err(|_| {
                        std::io::Error::other("the process being timed out never reported")
                    })?
                }
            };
            output.map(|output| (output, true))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(std::io::Error::other(
            "the thread waiting for this action disappeared",
        )),
    }
}

/// How long a timed-out process gets to exit after SIGTERM before it is killed
/// outright. Long enough for a runner to flush a report, short enough that the
/// build still returns.
const TIMEOUT_KILL_GRACE: Duration = Duration::from_secs(5);

#[cfg(unix)]
fn kill_process_tree(pid: u32) {
    // SAFETY: as terminate_process_tree, with the signal that cannot be caught.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_process_tree(pid: u32) {
    // taskkill /F is already forceful, so the escalation is the same call.
    terminate_process_tree(pid);
}

pub fn was_cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}
