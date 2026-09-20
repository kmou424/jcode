//! Client-side herdr lifecycle reporting for `--ssh` sessions.
//!
//! `jcode_app_core::herdr` reports agent states from the server process, but
//! under `jcode --ssh` the server runs on the remote host: `herdr` and the
//! pane id live on the local client, so a remote spawn can never reach the
//! multiplexer. The TUI is the process inside the pane and owns the real
//! `HERDR_*` environment, so this module re-derives the same lifecycle from
//! client state after each server event and reports it locally.
//!
//! The mapping mirrors the server side: an open ask_user_question panel is
//! `blocked` (the remote turn waits on the user), an active turn is
//! `working`, anything else is `idle`. Emission is edge-triggered through
//! `App::herdr_reported`, so one report goes out per transition rather than
//! per event, and a `remote_session_id` change additionally publishes the
//! session identity like the server-side attach report does.
//!
//! Only SSH mode uses this path. Local `jcode` runs the agent in-process and
//! `jcode connect` forwards the client's `HERDR_*` snapshot to the daemon, so
//! both are already covered server-side and client-side reporting would
//! double-report.

use jcode_app_core::herdr::{self, AgentState};

use super::App;

/// The herdr state implied by client-visible session state.
fn client_state(app: &App) -> AgentState {
    if app.inline_ask_user_question_state.is_some() {
        AgentState::Blocked
    } else if app.is_processing {
        AgentState::Working
    } else {
        AgentState::Idle
    }
}

/// Emit a herdr report when the derived client state or attached session
/// changed. Runs after each server event; a no-op outside SSH mode and, via
/// the env gate inside `jcode_app_core::herdr`, outside herdr panes.
pub(super) fn observe(app: &mut App) {
    if !crate::tui::is_ssh_remote() {
        return;
    }
    let session_id = app.remote_session_id.clone();
    let state = client_state(app);
    let session_changed = app
        .herdr_reported
        .as_ref()
        .map(|(_, last_session)| *last_session != session_id)
        .unwrap_or(true);
    if matches!(&app.herdr_reported, Some((last_state, _)) if *last_state == state)
        && !session_changed
    {
        return;
    }
    app.herdr_reported = Some((state, session_id.clone()));
    if session_changed {
        if let Some(id) = session_id.as_deref() {
            herdr::report_session_identity(id);
        }
    }
    let message = (state == AgentState::Blocked).then_some("awaiting answer");
    herdr::report_for_session(state, session_id.as_deref(), message);
}
