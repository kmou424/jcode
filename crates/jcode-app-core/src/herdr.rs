//! Best-effort agent lifecycle reporting to herdr (https://herdr.dev).
//!
//! When jcode runs inside a herdr pane, status transitions are reported with
//! `herdr pane report-agent --source custom:jcode --agent jcode` so the
//! multiplexer can show the agent's real state instead of guessing from
//! terminal output. Reporting is fully env-gated: without `HERDR_ENV=1` and
//! `HERDR_PANE_ID` every entry point is a no-op, and spawn failures only log.
//!
//! On the shared server the pane identity is not the daemon's environment:
//! each connecting client snapshots its `HERDR_*` vars (see
//! `terminal_launch::CLIENT_TERMINAL_ENV_VARS`) and request handlers run
//! inside `hooks::with_client_terminal_env`, so
//! `hooks::client_terminal_env_value` resolves the pane that actually hosts
//! this session.
//!
//! Status mapping (`report-agent` accepts idle|working|blocked|unknown;
//! there is no "done"/"stopped" state):
//!
//! | jcode status                        | herdr state |
//! |-------------------------------------|-------------|
//! | session create/attach/resume        | idle        |
//! | turn start (processing/streaming)   | working     |
//! | turn end (any outcome)              | idle        |
//! | `SessionStatus::AwaitingUser`       | blocked     |
//! | answer received, turn resumes       | working     |
//! | orphaned answer on a dead turn      | idle        |
//! | session end/close                   | idle        |
//!
//! A blocked session is still alive and resumable, so it is never reported
//! with a stopped-class state; `blocked` is the accurate verb. Normal
//! completion settles to `idle` since herdr distinguishes `idle`/`done`
//! itself via its own seen-state tracking.
//!
//! `report-agent` registrations are sticky: without `pane release-agent`
//! the pane keeps listing the agent after jcode exits. The exit path calls
//! `release_if_registered` so every pane-hosting entry point (interactive
//! TUI in local/connect/ssh mode, `jcode run`) drops its entry on the way
//! out. Uncatchable deaths (SIGKILL, panic abort) cannot release; the stale
//! entry only clears when the pane closes or a later report re-registers.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether this process has successfully spawned at least one herdr report.
/// `release_if_registered` consults it so unrelated invocations inside a
/// herdr pane (`jcode status`, `jcode doctor`, ...) do not release an agent
/// that a *different* live jcode process is reporting for the same pane.
static REGISTERED: AtomicBool = AtomicBool::new(false);

/// Agent lifecycle states jcode reports to herdr. `unknown` exists in herdr's
/// enum but is never emitted deliberately: it means "present but
/// unclassifiable", which a direct report always knows better than.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
}

impl AgentState {
    /// The herdr CLI state string for `report-agent --state`.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::Blocked => "blocked",
        }
    }
}

/// Resolved reporting target: the herdr binary and the pane to report for.
pub struct Target {
    /// `HERDR_BIN_PATH` when set, else `herdr` resolved through PATH.
    pub bin: String,
    /// `HERDR_PANE_ID` of the pane hosting this agent.
    pub pane_id: String,
}

/// Resolve the herdr binary and pane id from the current client terminal env.
/// `get` reads one variable, so tests can inject a map without touching the
/// process environment. Returns `None` (reporting disabled) unless herdr
/// marked this pane with `HERDR_ENV=1` and supplied `HERDR_PANE_ID`.
pub fn resolve_target(get: impl Fn(&str) -> Option<String>) -> Option<Target> {
    if get("HERDR_ENV").as_deref() != Some("1") {
        return None;
    }
    let pane_id = get("HERDR_PANE_ID")?;
    Some(Target {
        bin: get("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".to_string()),
        pane_id,
    })
}

fn current_target() -> Option<Target> {
    resolve_target(crate::hooks::client_terminal_env_value)
}

/// Spawn `herdr` detached; failure is log-only and never propagates.
fn spawn(cmd: &mut Command) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(_) => REGISTERED.store(true, Ordering::Relaxed),
        Err(error) => crate::logging::warn(&format!("herdr report-agent spawn failed: {error}")),
    }
}

/// Report an agent lifecycle state for the current pane. No-op outside herdr.
pub fn report(state: AgentState) {
    report_for_session(state, None, None)
}

/// Report a lifecycle state, optionally carrying the jcode session id and a
/// short human-readable message for the pane badge.
pub fn report_for_session(state: AgentState, session_id: Option<&str>, message: Option<&str>) {
    let Some(target) = current_target() else {
        return;
    };
    let mut cmd = Command::new(&target.bin);
    cmd.args([
        "pane",
        "report-agent",
        "--source",
        "custom:jcode",
        "--agent",
        "jcode",
        "--state",
        state.as_str(),
    ]);
    if let Some(id) = session_id.filter(|id| !id.is_empty()) {
        cmd.args(["--agent-session-id", id]);
    }
    if let Some(message) = message.filter(|message| !message.is_empty()) {
        cmd.args(["--message", message]);
    }
    cmd.arg(&target.pane_id);
    spawn(&mut cmd);
}

/// Release the pane's agent lifecycle authority, removing the entry from
/// `herdr agent list`. Fires when the pane-hosting process exits: the agent
/// is gone rather than merely idle, so `idle` is not the right final state.
/// No-op outside herdr.
pub fn release() {
    let Some(target) = current_target() else {
        return;
    };
    let mut cmd = Command::new(&target.bin);
    cmd.args([
        "pane",
        "release-agent",
        "--source",
        "custom:jcode",
        "--agent",
        "jcode",
    ]);
    cmd.arg(&target.pane_id);
    spawn(&mut cmd);
}

/// Release only when this process actually emitted a report. Called from the
/// process exit path, which every pane-hosting entry point (interactive TUI
/// in local/connect/ssh mode, `jcode run`, subcommands) flows through.
pub fn release_if_registered() {
    if REGISTERED.load(Ordering::Relaxed) {
        release();
    }
}

/// Test-only reset for the process-global spawn flag; integration tests run
/// one binary per file, so clearing it between cases keeps assertions
/// deterministic without serializing on execution order.
#[doc(hidden)]
pub fn __test_reset_registered() {
    REGISTERED.store(false, Ordering::Relaxed);
}

/// Publish the session identity for the current pane so herdr can correlate
/// the agent with the jcode session file. No-op outside herdr.
pub fn report_session_identity(session_id: &str) {
    let Some(target) = current_target() else {
        return;
    };
    if session_id.is_empty() {
        return;
    }
    let mut cmd = Command::new(&target.bin);
    cmd.args([
        "pane",
        "report-agent-session",
        "--source",
        "custom:jcode",
        "--agent",
        "jcode",
        "--agent-session-id",
        session_id,
    ]);
    cmd.arg(&target.pane_id);
    spawn(&mut cmd);
}
