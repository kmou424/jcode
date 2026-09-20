//! `ask_user_question` tool: a blocking interactive prompt that asks
//! the user 1-4 structured questions and waits for the answers.
//!
//! Live path (daemon-attached client): the tool records the pending question
//! on the session file, hands the request to `ctx.ask_user_question_tx`, and
//! parks on a oneshot. The per-connection forwarder registers it in the
//! session-scoped pending registry and emits `ServerEvent::AskUserQuestion`;
//! the client's `AskUserQuestionResponse` resolves the oneshot — including
//! after a disconnect/re-attach.
//!
//! Headless path (`jcode run`, or no attached interactive client): the tool
//! falls back to reading numbered answers from stdin lines.

use super::{AskUserQuestionRequest, Tool, ToolContext, ToolOutput};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use jcode_session_types::SessionStatus;
use jcode_session_types::ask_user_question::{
    AskUserQuestion, AskUserQuestionAnswer, AskUserQuestionResult, AskUserQuestionSelection,
    PendingAskUserQuestion, format_answers, normalize_questions, validate_questions,
};
use serde::Deserialize;
use serde_json::{Value, json};

const DESCRIPTION: &str = "\
Ask the user one or more structured questions and block until they answer. \
Use this when a decision genuinely needs the user's input: choosing between \
viable approaches, confirming a destructive action, or gathering preferences \
that change the outcome. Do not use it for information you can determine \
yourself. Each question lists up to 4 options; the client always appends a \
free-text \"Type something.\" option, so never author one yourself. Mark at \
most one option per question with `recommended: true` for the option you \
would pick.";

#[derive(Deserialize)]
struct AskUserQuestionInput {
    questions: Vec<AskUserQuestion>,
}

pub struct AskUserQuestionTool;

impl AskUserQuestionTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> &str {
        "ask_user_question"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 4,
                    "description": "Questions to ask, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The full question text shown to the user."
                            },
                            "header": {
                                "type": "string",
                                "description": "Short label for the question (max 12 chars, e.g. \"Approach\"). Defaults to Q1, Q2, ... when omitted."
                            },
                            "options": {
                                "type": "array",
                                "minItems": 1,
                                "maxItems": 4,
                                "description": "Selectable choices. A free-text row is appended by the client; do not author one.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "description": "Choice label (1-5 words, max 60 chars)."
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "One short sentence explaining the impact or trade-off."
                                        },
                                        "preview": {
                                            "type": "string",
                                            "description": "Optional Markdown detail shown beside the option on wide panes."
                                        },
                                        "recommended": {
                                            "type": "boolean",
                                            "description": "Mark this option as your recommendation (at most one per question)."
                                        }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multi_select": {
                                "type": "boolean",
                                "description": "Allow selecting multiple options (default false)."
                            }
                        },
                        "required": ["question", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: AskUserQuestionInput = serde_json::from_value(input)
            .map_err(|error| anyhow!("ask_user_question: invalid input: {error}"))?;
        let mut questions = params.questions;
        let errors = validate_questions(&questions);
        if !errors.is_empty() {
            return Err(anyhow!("ask_user_question: {}", errors.join("; ")));
        }
        normalize_questions(&mut questions);

        let request_id = crate::id::new_id("ask_user_question");
        // Persist before blocking so a crash, detach or resume can still
        // re-present the question and anchor a late answer to this tool call.
        record_pending_in_session(&ctx.session_id, &request_id, &ctx.tool_call_id, &questions);
        crate::herdr::report_for_session(
            crate::herdr::AgentState::Blocked,
            Some(&ctx.session_id),
            Some("awaiting answer"),
        );

        let result = if let Some(tx) = ctx.ask_user_question_tx.clone() {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let request = AskUserQuestionRequest {
                request_id,
                session_id: ctx.session_id.clone(),
                tool_call_id: ctx.tool_call_id.clone(),
                questions: questions.clone(),
                response_tx,
            };
            if tx.send(request).is_err() {
                // The forwarding task is gone (no interactive client). Fall
                // back to the stdin-lines path like a headless run.
                headless_collect(&questions).await
            } else {
                match response_rx.await {
                    Ok(result) => result,
                    // The registry entry was dropped (disconnect cleanup,
                    // replaced question, or server shutdown).
                    Err(_) => AskUserQuestionResult {
                        cancelled: true,
                        ..AskUserQuestionResult::default()
                    },
                }
            }
        } else {
            headless_collect(&questions).await
        };

        clear_pending_in_session(&ctx.session_id);
        // The answer (or cancellation) lets the turn continue, so the agent
        // is working again until turn_end settles it back to idle.
        crate::herdr::report_for_session(
            crate::herdr::AgentState::Working,
            Some(&ctx.session_id),
            None,
        );

        if result.cancelled {
            return Ok(ToolOutput::new("User cancelled the questionnaire"));
        }
        Ok(ToolOutput::new(format_answers(&questions, &result.answers)))
    }
}

/// Write the pending question onto the session file and mark the session as
/// awaiting a user answer. Best-effort: sessions without a loadable file
/// (ephemeral or in-flight ids) simply skip persistence — the in-memory
/// registry still routes the answer.
fn record_pending_in_session(
    session_id: &str,
    request_id: &str,
    tool_call_id: &str,
    questions: &[AskUserQuestion],
) {
    if session_id.is_empty() {
        return;
    }
    let record = PendingAskUserQuestion {
        request_id: request_id.to_string(),
        tool_call_id: if tool_call_id.is_empty() {
            None
        } else {
            Some(tool_call_id.to_string())
        },
        questions: questions.to_vec(),
        asked_at: chrono::Utc::now(),
    };
    match crate::session::Session::load(session_id) {
        Ok(mut session) => {
            session.pending_ask_user_question = Some(record);
            session.status = SessionStatus::AwaitingUser;
            if let Err(error) = session.save() {
                crate::logging::warn(&format!(
                    "ask_user_question: failed to persist pending question on session {session_id}: {error}"
                ));
            }
        }
        Err(error) => {
            crate::logging::warn(&format!(
                "ask_user_question: failed to load session {session_id} for pending persistence: {error}"
            ));
        }
    }
}

/// Clear the persisted pending record once the question resolves. The live
/// agent's in-memory session never carried it, so the next normal save would
/// drop it anyway; clearing eagerly keeps a crash between answer and next
/// save from resurrecting an already-answered question.
fn clear_pending_in_session(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    let Ok(mut session) = crate::session::Session::load(session_id) else {
        return;
    };
    if session.pending_ask_user_question.is_none() {
        return;
    }
    session.pending_ask_user_question = None;
    if session.status == SessionStatus::AwaitingUser {
        session.status = SessionStatus::Active;
    }
    if let Err(error) = session.save() {
        crate::logging::warn(&format!(
            "ask_user_question: failed to clear pending question on session {session_id}: {error}"
        ));
    }
}

/// Headless fallback: print each question with numbered options on stderr and
/// read one answer line per question from stdin. A plain number selects that
/// option, comma-separated numbers select multiple on a multi-select
/// question, and any other line is taken as free text. `cancel`, `quit`, or
/// EOF aborts the questionnaire.
async fn headless_collect(questions: &[AskUserQuestion]) -> AskUserQuestionResult {
    let questions = questions.to_vec();
    tokio::task::spawn_blocking(move || headless_collect_blocking(&questions))
        .await
        .unwrap_or_else(|_| AskUserQuestionResult {
            cancelled: true,
            ..AskUserQuestionResult::default()
        })
}

fn headless_collect_blocking(questions: &[AskUserQuestion]) -> AskUserQuestionResult {
    use std::io::Write;
    let stdin = std::io::stdin();
    let mut answers = Vec::new();
    for (question_index, question) in questions.iter().enumerate() {
        let header = question.display_header(question_index);
        eprintln!();
        eprintln!("[{header}] {}", question.question);
        let option_count = question.options.len();
        for (index, option) in question.options.iter().enumerate() {
            let description = option
                .description
                .as_deref()
                .map(|description| format!(" — {description}"))
                .unwrap_or_default();
            eprintln!("  {}. {}{}", index + 1, option.label, description);
        }
        eprintln!("  {}. Type something.", option_count + 1);
        let hint = if question.is_multi_select() {
            "Enter numbers separated by commas, or type an answer"
        } else {
            "Enter a number, or type an answer"
        };
        eprint!("{hint} [1-{}, or 'cancel']: ", option_count + 1);
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        let read = stdin.read_line(&mut line).unwrap_or(0);
        if read == 0 {
            return AskUserQuestionResult {
                cancelled: true,
                answers,
            };
        }
        let line = line.trim();
        if line.is_empty() || matches!(line.to_ascii_lowercase().as_str(), "cancel" | "quit" | "q")
        {
            return AskUserQuestionResult {
                cancelled: true,
                answers,
            };
        }

        let parse_index = |token: &str| -> Option<usize> {
            token
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|index| (1..=option_count).contains(index))
                .map(|index| index - 1)
        };

        if question.is_multi_select() {
            let tokens: Vec<&str> = line.split(',').collect();
            let selected: Vec<usize> = tokens
                .iter()
                .filter_map(|token| parse_index(token))
                .collect();
            if tokens.len() > 1 && !selected.is_empty() {
                answers.push(AskUserQuestionAnswer::Multi {
                    question_index,
                    selected: selected
                        .into_iter()
                        .map(|index| AskUserQuestionSelection {
                            index,
                            label: question.options[index].label.clone(),
                        })
                        .collect(),
                    notes: None,
                });
                continue;
            }
        }

        if let Some(index) = parse_index(line) {
            answers.push(AskUserQuestionAnswer::Option {
                question_index,
                index,
                label: question.options[index].label.clone(),
                notes: None,
            });
        } else {
            answers.push(AskUserQuestionAnswer::Custom {
                question_index,
                text: line.to_string(),
                notes: None,
            });
        }
    }
    AskUserQuestionResult {
        cancelled: false,
        answers,
    }
}
