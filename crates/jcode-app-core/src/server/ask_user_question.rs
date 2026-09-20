//! Session-scoped pending registry for the `ask_user_question` tool.
//!
//! Unlike `stdin_request` — whose prompts are transport-scoped and die with
//! the connection — a pending questionnaire belongs to the session: the turn
//! stays parked until an answer arrives, so the question must survive client
//! disconnect and be re-presented to the next attachment. Entries come from
//! the per-connection forwarder draining `ctx.ask_user_question_tx` (live
//! entries carrying the tool's oneshot) or from a restored session's
//! persisted `pending_ask_user_question` record (orphaned entries whose
//! answer is injected into session history as the recorded tool call's
//! result).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use jcode_session_types::SessionStatus;
use jcode_session_types::ask_user_question::{
    AskUserQuestion, AskUserQuestionAnswer, AskUserQuestionResult, PendingAskUserQuestion,
    format_answers,
};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};

use crate::agent::Agent;
use crate::message::{ContentBlock, Role};
use crate::protocol::ServerEvent;
use crate::tool::AskUserQuestionRequest;

/// A registered questionnaire awaiting the user's answer.
struct PendingEntry {
    request_id: String,
    /// Anchors the answer to its tool result when the turn that asked is
    /// gone and the entry was restored from the session file.
    tool_call_id: Option<String>,
    questions: Vec<AskUserQuestion>,
    /// The live blocked tool's responder. `None` marks an orphaned entry
    /// seeded from persisted session state; its answer is written into
    /// session history instead.
    resolution: Option<oneshot::Sender<AskUserQuestionResult>>,
}

/// Pending questionnaires keyed by session id. One entry per session: a
/// session's turn is single-threaded, so at most one question can be blocked
/// at a time. Replaced entries resolve their tool as cancelled through the
/// dropped sender.
static PENDING_ASK_USER_QUESTIONS: LazyLock<Mutex<HashMap<String, PendingEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn pending_map() -> std::sync::MutexGuard<'static, HashMap<String, PendingEntry>> {
    PENDING_ASK_USER_QUESTIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Drain tool-originated questions for one connection: register each in the
/// session-scoped map and forward the questionnaire to the client. Keeps
/// running while its channel lives — including past a disconnect — so a
/// retained turn's later questions are still registered for the next
/// attachment to answer.
pub(super) fn spawn_ask_user_question_forwarder(
    mut rx: mpsc::UnboundedReceiver<AskUserQuestionRequest>,
    client_event_tx: mpsc::UnboundedSender<ServerEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            let session_id = request.session_id.clone();
            {
                let mut pending = pending_map();
                pending.insert(
                    session_id.clone(),
                    PendingEntry {
                        request_id: request.request_id.clone(),
                        tool_call_id: if request.tool_call_id.is_empty() {
                            None
                        } else {
                            Some(request.tool_call_id.clone())
                        },
                        questions: request.questions.clone(),
                        resolution: Some(request.response_tx),
                    },
                );
            }
            let _ = client_event_tx.send(ServerEvent::AskUserQuestion {
                request_id: request.request_id,
                session_id,
                questions: request.questions,
            });
        }
    })
}

/// Whether the session currently has a registered pending question. The
/// disconnect path uses this to retain a blocked turn the same way a remote
/// client's `continue_on_disconnect` would.
pub(super) fn has_pending_ask_user_question(session_id: &str) -> bool {
    pending_map().contains_key(session_id)
}

/// Build the re-presentation event for a (re)attaching client, if the
/// session still has an unanswered question.
pub(super) fn pending_ask_user_question_event(session_id: &str) -> Option<ServerEvent> {
    let pending = pending_map();
    pending
        .get(session_id)
        .map(|entry| ServerEvent::AskUserQuestion {
            request_id: entry.request_id.clone(),
            session_id: session_id.to_string(),
            questions: entry.questions.clone(),
        })
}

/// Seed an orphaned entry from a session restored with a persisted pending
/// question (the turn that asked is gone, so the answer is injected into
/// history instead of resolving a channel). A live in-memory entry for the
/// same session always wins.
pub(super) fn seed_pending_ask_user_question(session_id: &str, record: &PendingAskUserQuestion) {
    let mut pending = pending_map();
    if pending.contains_key(session_id) {
        return;
    }
    pending.insert(
        session_id.to_string(),
        PendingEntry {
            request_id: record.request_id.clone(),
            tool_call_id: record.tool_call_id.clone(),
            questions: record.questions.clone(),
            resolution: None,
        },
    );
}

/// Route a client's questionnaire answers to the pending entry for its
/// session: resolve the live tool's oneshot when present, otherwise inject
/// the answers into session history as the recorded tool call's result.
pub(super) async fn handle_ask_user_question_response(
    id: u64,
    client_session_id: &str,
    request_id: String,
    cancelled: bool,
    answers: Vec<AskUserQuestionAnswer>,
    agent: &Arc<AsyncMutex<Agent>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let entry = {
        let mut pending = pending_map();
        let matches = pending
            .get(client_session_id)
            .is_some_and(|entry| entry.request_id == request_id);
        if matches {
            pending.remove(client_session_id)
        } else {
            None
        }
    };

    let Some(mut entry) = entry else {
        crate::logging::info(&format!(
            "ask_user_question: ignoring response for unknown or stale request {request_id} on session {client_session_id}"
        ));
        let _ = client_event_tx.send(ServerEvent::Done { id });
        return;
    };

    let result = AskUserQuestionResult { cancelled, answers };
    if let Some(response_tx) = entry.resolution.take()
        && response_tx.send(result.clone()).is_ok()
    {
        let _ = client_event_tx.send(ServerEvent::Done { id });
        return;
    }

    // Orphaned entry, or a live entry whose turn went away before the answer
    // arrived: record the answer into session history so the model sees it
    // on the next turn instead of losing it.
    inject_orphaned_answer(client_session_id, &entry, &result, agent).await;
    let _ = client_event_tx.send(ServerEvent::Done { id });
}

/// Persist an answered-or-cancelled orphaned questionnaire into the session:
/// a synthetic `ToolResult` anchored to the recorded tool call when known,
/// otherwise a plain user message. Clears the persisted pending record.
async fn inject_orphaned_answer(
    session_id: &str,
    entry: &PendingEntry,
    result: &AskUserQuestionResult,
    agent: &Arc<AsyncMutex<Agent>>,
) {
    let content = if result.cancelled {
        "User cancelled the questionnaire".to_string()
    } else {
        format_answers(&entry.questions, &result.answers)
    };
    let block = if let Some(tool_call_id) = entry.tool_call_id.clone() {
        ContentBlock::ToolResult {
            tool_use_id: tool_call_id,
            content,
            is_error: None,
        }
    } else {
        ContentBlock::Text {
            text: format!("Questionnaire answer:\n{content}"),
            cache_control: None,
        }
    };

    let mut agent_guard = agent.lock().await;
    if agent_guard.session_id() != session_id {
        crate::logging::warn(&format!(
            "ask_user_question: orphaned answer for {session_id} reached a different agent's session {}; writing to the session file directly",
            agent_guard.session_id()
        ));
        drop(agent_guard);
        let Ok(mut session) = crate::session::Session::load(session_id) else {
            return;
        };
        session.add_message(Role::User, vec![block]);
        session.pending_ask_user_question = None;
        if session.status == SessionStatus::AwaitingUser {
            session.status = SessionStatus::Active;
        }
        let _ = session.save();
        // The blocked turn is gone; the session is back at the prompt.
        crate::herdr::report_for_session(crate::herdr::AgentState::Idle, Some(session_id), None);
        return;
    }
    let session = agent_guard.session_mut();
    session.add_message(Role::User, vec![block]);
    session.pending_ask_user_question = None;
    if session.status == SessionStatus::AwaitingUser {
        session.status = SessionStatus::Active;
    }
    if let Err(error) = session.save() {
        crate::logging::warn(&format!(
            "ask_user_question: failed to persist orphaned answer on session {session_id}: {error}"
        ));
    }
    crate::herdr::report_for_session(crate::herdr::AgentState::Idle, Some(session_id), None);
}
