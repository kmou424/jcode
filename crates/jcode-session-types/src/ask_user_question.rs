//! Shared `ask_user_question` tool types.
//!
//! The blocking interactive tool sends a normalized question list to
//! the attached client, which renders the questionnaire and posts back the
//! answers. These types cross the wire (jcode-protocol), persist into session
//! state (jcode-base `Session::pending_ask_user_question`), and feed both the
//! tool implementation (jcode-app-core) and the questionnaire UI
//! (jcode-tui), so they live in this leaf crate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MIN_QUESTIONS: usize = 1;
pub const MAX_QUESTIONS: usize = 4;
pub const MIN_OPTIONS: usize = 1;
pub const MAX_OPTIONS: usize = 4;
pub const MAX_HEADER_LENGTH: usize = 12;
pub const MAX_LABEL_LENGTH: usize = 60;

/// Client-painted recommendation tag; also the only suffix stripped from
/// authored labels.
pub const RECOMMENDED_SUFFIX: &str = " (Recommended)";

/// The client-appended free-text row; the model must not author it.
pub const FREE_TEXT_LABEL: &str = "Type something.";

/// Labels the model must not author as options. "Other" is reserved for
/// Codex parity; the free-text row is appended by the client.
pub const RESERVED_LABELS: &[&str] = &["Other", FREE_TEXT_LABEL];

/// One selectable choice authored by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestionOption {
    /// User-facing label (1-5 words, <= 60 chars).
    pub label: String,
    /// One short sentence explaining the impact or trade-off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional Markdown detail rendered beside the options on wide panes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Marks the option as the agent's recommendation (at most one per
    /// question). The client moves it first and stamps `(Recommended)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<bool>,
}

/// One question with its authored option list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestion {
    /// Markdown prompt shown to the user.
    pub question: String,
    /// Short header label (<= 12 chars); defaults to Q1, Q2, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// 1-4 authored options; the client appends the free-text row itself.
    pub options: Vec<AskUserQuestionOption>,
    /// Allow selecting multiple options instead of one (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
}

impl AskUserQuestion {
    /// Header after defaults are applied: the authored header or `Q{n}`.
    pub fn display_header(&self, index: usize) -> String {
        self.header
            .clone()
            .unwrap_or_else(|| format!("Q{}", index + 1))
    }

    pub fn is_multi_select(&self) -> bool {
        self.multi_select == Some(true)
    }
}

/// Strip the client-painted `(Recommended)` suffix a model may have baked
/// into a label.
pub fn strip_recommended_suffix(label: &str) -> &str {
    label.strip_suffix(RECOMMENDED_SUFFIX).unwrap_or(label)
}

/// Apply runtime defaults in place: headers `Q1..Q4`, strip the recommended
/// suffix from labels, and move the recommended option to the front.
pub fn normalize_questions(questions: &mut Vec<AskUserQuestion>) {
    for (index, question) in questions.iter_mut().enumerate() {
        if question.header.is_none() {
            question.header = Some(format!("Q{}", index + 1));
        }
        for option in question.options.iter_mut() {
            option.label = strip_recommended_suffix(&option.label).to_string();
            if option.recommended != Some(true) {
                option.recommended = None;
            }
        }
        if let Some(recommended_index) = question
            .options
            .iter()
            .position(|option| option.recommended == Some(true))
            && recommended_index > 0
        {
            let recommended = question.options.remove(recommended_index);
            question.options.insert(0, recommended);
        }
    }
}

/// Runtime guard rails (the schema bounds are advisory). Returns one error
/// string per violated invariant; an empty vec means the params are valid.
pub fn validate_questions(questions: &[AskUserQuestion]) -> Vec<String> {
    let mut errors = Vec::new();
    if questions.len() < MIN_QUESTIONS || questions.len() > MAX_QUESTIONS {
        errors.push(format!(
            "expected {MIN_QUESTIONS}-{MAX_QUESTIONS} questions, got {}",
            questions.len()
        ));
    }
    for (index, question) in questions.iter().enumerate() {
        let header = question.display_header(index);
        let prefix = format!("question {} ({header})", index + 1);
        if question.options.len() < MIN_OPTIONS || question.options.len() > MAX_OPTIONS {
            errors.push(format!(
                "{prefix}: expected {MIN_OPTIONS}-{MAX_OPTIONS} options, got {}",
                question.options.len()
            ));
        }
        let recommended_count = question
            .options
            .iter()
            .filter(|option| option.recommended == Some(true))
            .count();
        if recommended_count > 1 {
            errors.push(format!(
                "{prefix}: at most one recommended option, got {recommended_count}"
            ));
        }
        if let Some(header) = question.header.as_deref()
            && header.chars().count() > MAX_HEADER_LENGTH
        {
            errors.push(format!(
                "{prefix}: header \"{header}\" is {} chars (max {MAX_HEADER_LENGTH})",
                header.chars().count()
            ));
        }
        for option in &question.options {
            if strip_recommended_suffix(&option.label).is_empty() {
                errors.push(format!(
                    "{prefix}: option label is empty after removing the {RECOMMENDED_SUFFIX} suffix"
                ));
            }
            if option.label.chars().count() > MAX_LABEL_LENGTH {
                errors.push(format!(
                    "{prefix}: option label \"{}\" is {} chars (max {MAX_LABEL_LENGTH})",
                    option.label,
                    option.label.chars().count()
                ));
            }
            if RESERVED_LABELS.contains(&option.label.as_str()) {
                errors.push(format!(
                    "{prefix}: option label \"{}\" is reserved (the client appends the free-text row automatically)",
                    option.label
                ));
            }
        }
    }
    errors
}

/// One committed selection inside a multi-select answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestionSelection {
    /// 0-based option position in the normalized list.
    pub index: usize,
    pub label: String,
}

/// Committed answer for one question. `index`/`selected[].index` are 0-based
/// option positions; `notes` is an optional pre-answer note attached by the
/// user at confirm time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskUserQuestionAnswer {
    Option {
        question_index: usize,
        index: usize,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
    },
    Multi {
        question_index: usize,
        selected: Vec<AskUserQuestionSelection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
    },
    Custom {
        question_index: usize,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<String>,
    },
}

impl AskUserQuestionAnswer {
    pub fn question_index(&self) -> usize {
        match self {
            Self::Option { question_index, .. }
            | Self::Multi { question_index, .. }
            | Self::Custom { question_index, .. } => *question_index,
        }
    }

    pub fn notes(&self) -> Option<&str> {
        match self {
            Self::Option { notes, .. } | Self::Multi { notes, .. } | Self::Custom { notes, .. } => {
                notes.as_deref()
            }
        }
    }
}

/// Outcome delivered to the tool when the questionnaire closes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestionResult {
    /// True when the user aborted (Esc/Ctrl-C) instead of submitting.
    #[serde(default)]
    pub cancelled: bool,
    /// Answers in question order; skipped questions are absent.
    #[serde(default)]
    pub answers: Vec<AskUserQuestionAnswer>,
}

/// A question the agent asked that is still awaiting a user answer.
///
/// Persisted on `Session` so a resume can re-present it: the live flow keeps
/// an in-memory copy plus the response channel, while this record is what
/// survives a daemon restart. `tool_call_id` anchors the interrupted tool
/// call so a late answer can be injected as its tool result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingAskUserQuestion {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub questions: Vec<AskUserQuestion>,
    pub asked_at: DateTime<Utc>,
}

/// Render answers the way the model sees them in the tool result: one line per
/// answered question as `"<header>: user selected: N. label"`,
/// `"<header>: user selected: label1, label2"` for multi-select, or
/// `"<header>: user wrote: <text>"` for free text. Optional notes attach in
/// parentheses. The server reuses this when a late answer has to be injected
/// into session history as a synthetic tool result.
pub fn format_answers(questions: &[AskUserQuestion], answers: &[AskUserQuestionAnswer]) -> String {
    answers
        .iter()
        .map(|answer| {
            let question_index = answer.question_index();
            let header = questions
                .get(question_index)
                .map(|question| question.display_header(question_index))
                .unwrap_or_else(|| format!("Q{}", question_index + 1));
            let notes = answer
                .notes()
                .map(|notes| format!(" ({notes})"))
                .unwrap_or_default();
            match answer {
                AskUserQuestionAnswer::Option { index, label, .. } => {
                    format!("{header}: user selected: {}. {label}{notes}", index + 1)
                }
                AskUserQuestionAnswer::Multi { selected, .. } => {
                    let labels = selected
                        .iter()
                        .map(|selection| selection.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{header}: user selected: {labels}{notes}")
                }
                AskUserQuestionAnswer::Custom { text, .. } => {
                    format!("{header}: user wrote: {text}{notes}")
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(label: &str) -> AskUserQuestionOption {
        AskUserQuestionOption {
            label: label.to_string(),
            description: None,
            preview: None,
            recommended: None,
        }
    }

    fn question(header: Option<&str>, options: Vec<AskUserQuestionOption>) -> AskUserQuestion {
        AskUserQuestion {
            question: "pick one".to_string(),
            header: header.map(str::to_string),
            options,
            multi_select: None,
        }
    }

    #[test]
    fn normalize_fills_default_headers() {
        let mut questions = vec![
            question(None, vec![option("a")]),
            question(Some("Auth"), vec![option("b")]),
            question(None, vec![option("c")]),
        ];
        normalize_questions(&mut questions);
        assert_eq!(questions[0].header.as_deref(), Some("Q1"));
        assert_eq!(questions[1].header.as_deref(), Some("Auth"));
        assert_eq!(questions[2].header.as_deref(), Some("Q3"));
    }

    #[test]
    fn normalize_moves_recommended_option_first() {
        let mut questions = vec![question(
            None,
            vec![
                option("first"),
                AskUserQuestionOption {
                    recommended: Some(true),
                    ..option("better")
                },
                option("third"),
            ],
        )];
        normalize_questions(&mut questions);
        let labels: Vec<&str> = questions[0]
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        assert_eq!(labels, vec!["better", "first", "third"]);
        assert_eq!(questions[0].options[0].recommended, Some(true));
        assert_eq!(questions[0].options[1].recommended, None);
    }

    #[test]
    fn normalize_strips_authored_recommended_suffix() {
        let mut questions = vec![question(
            None,
            vec![option("alpha (Recommended)"), option("beta")],
        )];
        normalize_questions(&mut questions);
        assert_eq!(questions[0].options[0].label, "alpha");
    }

    #[test]
    fn validate_rejects_out_of_range_shapes() {
        assert!(!validate_questions(&[]).is_empty());
        assert!(!validate_questions(&[question(None, vec![])]).is_empty());
        assert!(
            !validate_questions(&[question(
                None,
                vec![
                    option("a"),
                    option("b"),
                    option("c"),
                    option("d"),
                    option("e")
                ]
            )])
            .is_empty()
        );
        assert!(validate_questions(&[question(None, vec![option("a")])]).is_empty());
    }

    #[test]
    fn validate_rejects_duplicate_recommended_and_long_headers() {
        let mut dup = question(
            None,
            vec![
                AskUserQuestionOption {
                    recommended: Some(true),
                    ..option("a")
                },
                AskUserQuestionOption {
                    recommended: Some(true),
                    ..option("b")
                },
            ],
        );
        // normalize would collapse flags, so validate the raw authored shape.
        dup.header = Some("x".repeat(MAX_HEADER_LENGTH + 1));
        let errors = validate_questions(&[dup]);
        assert!(errors.iter().any(|e| e.contains("recommended")));
        assert!(errors.iter().any(|e| e.contains("header")));
    }

    #[test]
    fn format_answers_renders_all_kinds() {
        let mut questions = vec![
            question(None, vec![option("a"), option("b")]),
            question(Some("Multi"), vec![option("x"), option("y"), option("z")]),
            question(None, vec![option("only")]),
        ];
        normalize_questions(&mut questions);
        let answers = vec![
            AskUserQuestionAnswer::Option {
                question_index: 0,
                index: 1,
                label: "b".to_string(),
                notes: Some("picked b".to_string()),
            },
            AskUserQuestionAnswer::Multi {
                question_index: 1,
                selected: vec![
                    AskUserQuestionSelection {
                        index: 0,
                        label: "x".to_string(),
                    },
                    AskUserQuestionSelection {
                        index: 2,
                        label: "z".to_string(),
                    },
                ],
                notes: None,
            },
            AskUserQuestionAnswer::Custom {
                question_index: 2,
                text: "my own words".to_string(),
                notes: None,
            },
        ];
        let rendered = format_answers(&questions, &answers);
        assert_eq!(
            rendered,
            "Q1: user selected: 2. b (picked b)\nMulti: user selected: x, z\nQ3: user wrote: my own words"
        );
    }

    #[test]
    fn result_round_trips_through_json() {
        let result = AskUserQuestionResult {
            cancelled: true,
            answers: vec![AskUserQuestionAnswer::Custom {
                question_index: 0,
                text: "t".to_string(),
                notes: None,
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: AskUserQuestionResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, result);
    }

    #[test]
    fn pending_record_round_trips_through_json() {
        let record = PendingAskUserQuestion {
            request_id: "ask_user_question_1".to_string(),
            tool_call_id: Some("call_1".to_string()),
            questions: vec![question(Some("H"), vec![option("a")])],
            asked_at: Utc::now(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let back: PendingAskUserQuestion = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn question_json_omits_defaults() {
        let q = question(None, vec![option("a")]);
        let json = serde_json::to_value(&q).unwrap();
        assert!(json.get("header").is_none());
        assert!(json.get("multi_select").is_none());
        assert!(json["options"][0].get("recommended").is_none());
        assert!(json["options"][0].get("description").is_none());
    }
}
