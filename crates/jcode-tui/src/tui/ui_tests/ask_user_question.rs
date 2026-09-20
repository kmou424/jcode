//! Tests for the blocking `ask_user_question` questionnaire panel:
//! tab switching, preview pane, wrapping, the notes editor, the submit tab,
//! and composer hiding.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};
use jcode_session_types::{AskUserQuestion, AskUserQuestionOption};
use ratatui::backend::TestBackend;
use ratatui::{Terminal, layout::Rect};

fn option(label: &str) -> AskUserQuestionOption {
    AskUserQuestionOption {
        label: label.to_string(),
        description: None,
        preview: None,
        recommended: None,
    }
}

#[test]
fn multi_select_space_toggles_enter_commits() {
    let mut state = state_with(vec![multi_question(
        "Pick several?",
        vec![option("A"), option("B"), option("C")],
    )]);
    // No trailing Done row: options + free-text row only.
    assert_eq!(state.row_count(), 4);
    assert_eq!(state.row_kind(3), crate::tui::AskUserQuestionRow::Custom);

    // Space toggles check/uncheck on option rows.
    assert!(key(&mut state, KeyCode::Char(' ')).is_none());
    assert!(key(&mut state, KeyCode::Down).is_none());
    assert!(key(&mut state, KeyCode::Char(' ')).is_none());

    let lines = render_panel(&state, 60, 20);
    assert!(lines.iter().any(|l| l.contains("[✔] 1. A")));
    assert!(lines.iter().any(|l| l.contains("[✔] 2. B")));
    assert!(!lines.iter().any(|l| l.contains("Done")));

    // Enter is Done: commits the toggled set.
    let result = key(&mut state, KeyCode::Enter).expect("Enter should finish");
    assert!(!result.cancelled);
    assert_eq!(result.answers.len(), 1);
    match &result.answers[0] {
        jcode_session_types::AskUserQuestionAnswer::Multi { selected, .. } => {
            assert_eq!(selected.len(), 2);
            assert_eq!(selected[0].index, 0);
            assert_eq!(selected[1].index, 1);
        }
        other => panic!("expected Multi answer, got {other:?}"),
    }
}

#[test]
fn multi_select_enter_on_custom_row_commits() {
    let mut state = state_with(vec![multi_question(
        "Pick several?",
        vec![option("A"), option("B")],
    )]);
    assert!(key(&mut state, KeyCode::Char(' ')).is_none());
    // Land on the free-text row and press Enter: still Done, not editing.
    assert!(key(&mut state, KeyCode::Down).is_none());
    assert!(key(&mut state, KeyCode::Down).is_none());
    assert_eq!(
        state.row_kind(state.row_index),
        crate::tui::AskUserQuestionRow::Custom
    );
    let result = key(&mut state, KeyCode::Enter).expect("Enter should finish");
    assert!(!result.cancelled);
    assert!(!state.editing_custom);
}

#[test]
fn multi_select_enter_with_empty_selection_is_noop() {
    let mut state = state_with(vec![multi_question("Pick several?", vec![option("A")])]);
    assert!(key(&mut state, KeyCode::Enter).is_none());
    assert!(key(&mut state, KeyCode::Esc).is_some());
}

fn question(text: &str, options: Vec<AskUserQuestionOption>) -> AskUserQuestion {
    AskUserQuestion {
        question: text.to_string(),
        header: None,
        options,
        multi_select: None,
    }
}

fn multi_question(text: &str, options: Vec<AskUserQuestionOption>) -> AskUserQuestion {
    AskUserQuestion {
        question: text.to_string(),
        header: None,
        options,
        multi_select: Some(true),
    }
}

fn key(
    state: &mut crate::tui::InlineAskUserQuestionState,
    code: KeyCode,
) -> Option<jcode_session_types::AskUserQuestionResult> {
    match state.handle_key(code, KeyModifiers::NONE) {
        crate::tui::AskUserQuestionOutcome::Stay => None,
        crate::tui::AskUserQuestionOutcome::Finish(result) => Some(result),
    }
}

fn state_with(questions: Vec<AskUserQuestion>) -> crate::tui::InlineAskUserQuestionState {
    crate::tui::InlineAskUserQuestionState::new("req-1".to_string(), questions)
}

fn render_panel(
    state: &crate::tui::InlineAskUserQuestionState,
    width: u16,
    height: u16,
) -> Vec<String> {
    let mut test_state = TestState {
        inline_ask_user_question_state: Some(state.clone()),
        ..Default::default()
    };
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            crate::tui::ui::inline_ui::draw_inline_ui(
                frame,
                &test_state,
                Rect::new(0, 0, width, height),
            );
        })
        .expect("draw");
    let _ = &mut test_state;
    let buf = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn tab_and_arrows_switch_question_and_submit_tabs() {
    let mut state = state_with(vec![
        question("Q1?", vec![option("A"), option("B")]),
        question("Q2?", vec![option("A"), option("B")]),
    ]);
    assert_eq!(state.question_index, 0);
    assert!(key(&mut state, KeyCode::Tab).is_none());
    assert_eq!(state.question_index, 1);
    assert!(key(&mut state, KeyCode::Right).is_none());
    // Past the last question tab sits the submit tab.
    assert_eq!(state.question_index, 2);
    assert!(state.on_submit_tab());
    // Wraps back to the first question.
    assert!(key(&mut state, KeyCode::Tab).is_none());
    assert_eq!(state.question_index, 0);
    // Shift+Tab wraps the other way.
    assert!(key(&mut state, KeyCode::BackTab).is_none());
    assert_eq!(state.question_index, 2);
    assert!(key(&mut state, KeyCode::Left).is_none());
    assert_eq!(state.question_index, 1);
}

#[test]
fn tab_bar_and_submit_tab_render() {
    let mut state = state_with(vec![
        question("First?", vec![option("Alpha"), option("Beta")]),
        question("Second?", vec![option("Gamma"), option("Delta")]),
    ]);
    let lines = render_panel(&state, 100, 24).join("\n");
    assert!(lines.contains("Q1"), "{lines}");
    assert!(lines.contains("Q2"), "{lines}");
    assert!(lines.contains("Submit"), "{lines}");

    // Move to the submit tab: summary rows and the Submit/Cancel chooser.
    key(&mut state, KeyCode::Tab);
    key(&mut state, KeyCode::Tab);
    let lines = render_panel(&state, 100, 24).join("\n");
    assert!(lines.contains("Ready to submit"), "{lines}");
    assert!(lines.contains("unanswered"), "{lines}");
    assert!(lines.contains("> Submit"), "{lines}");
    assert!(lines.contains("  Cancel"), "{lines}");
}

#[test]
fn submit_tab_enter_submits_and_cancel_cancels() {
    let mut state = state_with(vec![
        question("First?", vec![option("Alpha"), option("Beta")]),
        question("Second?", vec![option("Gamma"), option("Delta")]),
    ]);
    key(&mut state, KeyCode::Tab);
    key(&mut state, KeyCode::Tab);
    assert!(state.on_submit_tab());
    let result = key(&mut state, KeyCode::Enter).expect("enter submits");
    assert!(!result.cancelled);

    let mut state = state_with(vec![
        question("First?", vec![option("Alpha"), option("Beta")]),
        question("Second?", vec![option("Gamma"), option("Delta")]),
    ]);
    key(&mut state, KeyCode::Tab);
    key(&mut state, KeyCode::Tab);
    key(&mut state, KeyCode::Down);
    let result = key(&mut state, KeyCode::Enter).expect("enter cancels");
    assert!(result.cancelled);
}

#[test]
fn esc_cancels_the_questionnaire() {
    let mut state = state_with(vec![question("Only?", vec![option("A")])]);
    let result = key(&mut state, KeyCode::Esc).expect("esc cancels");
    assert!(result.cancelled);
    assert!(result.answers.is_empty());
}

#[test]
fn preview_pane_renders_markdown_beside_options() {
    let mut previewed = option("With preview");
    previewed.preview = Some("**bold** detail\nsecond line".to_string());
    let mut plain = option("No preview");
    plain.preview = Some("".to_string());
    let state = state_with(vec![question("Pick?", vec![previewed, plain])]);
    let lines = render_panel(&state, 140, 24);
    let text = lines.join("\n");
    // Preview box chrome and the rendered markdown both show on the right.
    assert!(text.contains("┌"), "{text}");
    assert!(text.contains("│"), "{text}");
    assert!(text.contains("bold"), "{text}");
    assert!(text.contains("second line"), "{text}");
    assert!(text.contains("Notes: press n to add notes"), "{text}");
}

#[test]
fn preview_pane_shows_placeholder_without_preview_field() {
    let mut previewed = option("With preview");
    previewed.preview = Some("has one".to_string());
    let state = state_with(vec![question("Pick?", vec![previewed, option("Plain")])]);
    // Focus the option without a preview.
    let mut state = state;
    state.row_index = 1;
    let text = render_panel(&state, 140, 24).join("\n");
    assert!(text.contains("No preview available"), "{text}");
}

#[test]
fn long_option_text_and_description_wrap_within_the_column() {
    let mut opt = option(&"very long option label that keeps going ".repeat(3));
    opt.description = Some("a description sentence that is also quite long and must wrap".into());
    let state = state_with(vec![question("Pick?", vec![opt])]);
    let width = 60u16;
    let lines = render_panel(&state, width, 24);
    for (i, line) in lines.iter().enumerate() {
        assert!(
            unicode_width::UnicodeWidthStr::width(line.as_str()) <= width as usize,
            "row {i} overruns the panel width: {line:?}"
        );
    }
    // The label visibly continues on a wrapped row rather than truncating.
    let text = lines.join("\n");
    assert!(text.contains("keeps"), "{text}");
    assert!(text.contains("wrap"), "{text}");
}

#[test]
fn notes_editor_editing_and_merge_into_answer() {
    let mut state = state_with(vec![question("Pick?", vec![option("A"), option("B")])]);
    // Open notes with `n`.
    assert!(key(&mut state, KeyCode::Char('n')).is_none());
    assert!(state.editing_notes);
    for c in "hello world".chars() {
        key(&mut state, KeyCode::Char(c));
    }
    // Cursor left, then Alt+Backspace deletes the word backward.
    key(&mut state, KeyCode::Left);
    key(&mut state, KeyCode::Left);
    assert_eq!(state.notes_text, "hello world");
    match state.handle_key(KeyCode::Backspace, KeyModifiers::ALT) {
        crate::tui::AskUserQuestionOutcome::Stay => {}
        _ => panic!("alt-backspace should stay"),
    }
    // Cursor sits after "hello "; word-backward removes "wor".
    assert_eq!(state.notes_text, "hello ld");
    // Enter saves the note.
    key(&mut state, KeyCode::Enter);
    assert!(!state.editing_notes);
    assert_eq!(state.notes_by_question[0].as_deref(), Some("hello ld"));
    // Answering merges the note into the wire answer.
    key(&mut state, KeyCode::Down); // focus option B
    let result = key(&mut state, KeyCode::Enter).expect("single question finishes");
    assert!(!result.cancelled);
    match &result.answers[0] {
        jcode_session_types::AskUserQuestionAnswer::Option { index, notes, .. } => {
            assert_eq!(*index, 1);
            assert_eq!(notes.as_deref(), Some("hello ld"));
        }
        other => panic!("expected option answer, got {other:?}"),
    }
}

#[test]
fn notes_editor_cursor_moves_and_inserts_mid_text() {
    let mut state = state_with(vec![question("Pick?", vec![option("A")])]);
    key(&mut state, KeyCode::Char('n'));
    for c in "ab".chars() {
        key(&mut state, KeyCode::Char(c));
    }
    key(&mut state, KeyCode::Left);
    key(&mut state, KeyCode::Char('X'));
    assert_eq!(state.notes_text, "aXb");
    // Normal Backspace deletes the char before the cursor.
    key(&mut state, KeyCode::Backspace);
    assert_eq!(state.notes_text, "ab");
    // Esc also saves the note.
    key(&mut state, KeyCode::Esc);
    assert_eq!(state.notes_by_question[0].as_deref(), Some("ab"));
}

#[test]
fn custom_row_editing_supports_cursor_keys() {
    let mut state = state_with(vec![question("Pick?", vec![option("A")])]);
    // Landing on the free-text row enters edit mode on its own (no Enter
    // needed), matching the wow-pi inline editor.
    key(&mut state, KeyCode::Down);
    assert!(state.editing_custom);
    // Moving back off the row leaves edit mode; the draft is kept.
    key(&mut state, KeyCode::Up);
    assert!(!state.editing_custom);
    key(&mut state, KeyCode::Down);
    assert!(state.editing_custom);
    for c in "ac".chars() {
        key(&mut state, KeyCode::Char(c));
    }
    key(&mut state, KeyCode::Left);
    key(&mut state, KeyCode::Char('b'));
    assert_eq!(state.custom_text, "abc");
    let result = key(&mut state, KeyCode::Enter).expect("enter submits");
    match &result.answers[0] {
        jcode_session_types::AskUserQuestionAnswer::Custom { text, .. } => {
            assert_eq!(text, "abc");
        }
        other => panic!("expected custom answer, got {other:?}"),
    }
}

#[test]
fn composer_is_hidden_while_panel_is_open() {
    let mut test_state = TestState {
        input: "draft".to_string(),
        ..Default::default()
    };
    let render = |state: &TestState| {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| crate::tui::ui::draw(frame, state))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..30)
            .map(|y| (0..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let before = render(&test_state);
    // The composer (prompt number + draft text) is visible without the panel.
    assert!(before.contains("draft"), "{before}");
    test_state.inline_ask_user_question_state = Some(state_with(vec![question(
        "Pick?",
        vec![option("A"), option("B")],
    )]));
    let during = render(&test_state);
    assert!(during.contains("Pick?"), "{during}");
    // The composer line is gone while the panel is open.
    assert!(!during.contains("draft"), "{during}");
    test_state.inline_ask_user_question_state = None;
    let after = render(&test_state);
    assert!(after.contains("draft"), "{after}");
}

fn render_buffer(
    state: &crate::tui::InlineAskUserQuestionState,
    width: u16,
    height: u16,
) -> ratatui::buffer::Buffer {
    let test_state = TestState {
        inline_ask_user_question_state: Some(state.clone()),
        ..Default::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            crate::tui::ui::inline_ui::draw_inline_ui(
                frame,
                &test_state,
                Rect::new(0, 0, width, height),
            );
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

fn buffer_row_text(buf: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
    (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>()
}

/// Landing on the free-text row enters editing with an empty draft; the
/// `N.` number must stay and the reversed-video cursor cell must render
/// even though the draft has no text yet.
#[test]
fn custom_row_editing_keeps_number_and_shows_cursor_on_empty_draft() {
    let mut state = state_with(vec![question("Pick?", vec![option("A"), option("B")])]);
    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Down);
    assert!(state.editing_custom);
    assert!(state.custom_text.is_empty());

    let buf = render_buffer(&state, 60, 20);
    let custom_y = (0..20u16)
        .find(|&y| buffer_row_text(&buf, y, 60).contains("3."))
        .expect("custom row keeps its `3.` number while editing");
    assert!(
        buffer_row_text(&buf, custom_y, 60).contains('█'),
        "empty draft should still render a block cursor"
    );
}

/// `n` opens the notes editor with an empty draft; the cursor cell is a
/// `█` glyph — a reversed space was dropped by the markdown wrapper as a
/// blank line, hiding the cursor entirely.
#[test]
fn notes_editor_empty_draft_shows_cursor() {
    let mut state = state_with(vec![question("Pick?", vec![option("A")])]);
    key(&mut state, KeyCode::Char('n'));
    assert!(state.editing_notes);
    assert!(state.notes_text.is_empty());

    let buf = render_buffer(&state, 60, 20);
    let note_y = (0..20u16)
        .find(|&y| buffer_row_text(&buf, y, 60).contains("Note:"))
        .expect("notes block renders");
    assert!(
        (note_y..20u16).any(|y| buffer_row_text(&buf, y, 60).contains('█')),
        "empty notes draft should render a block cursor"
    );
}

/// When the option list is taller than the panel, the question header
/// stays pinned at the top and the viewport follows the selection so
/// off-screen options — and the free-text row — become reachable.
#[test]
fn options_scroll_to_follow_selection() {
    let options: Vec<_> = (1..=12).map(|i| option(&format!("Option {i}"))).collect();
    let mut state = state_with(vec![question("Pick one?", options)]);

    // Cursor on the first option: the top of the list shows.
    let text = render_panel(&state, 60, 12).join("\n");
    assert!(text.contains("Pick one?"), "{text}");
    assert!(text.contains("> 1. Option 1"), "{text}");
    assert!(!text.contains("Option 12"), "{text}");
    // Key hints stay pinned at the bottom.
    assert!(text.contains("Esc cancel"), "{text}");

    // Walk to the last option: the viewport must follow.
    for _ in 0..11 {
        key(&mut state, KeyCode::Down);
    }
    assert_eq!(state.row_index, 11);
    let text = render_panel(&state, 60, 12).join("\n");
    assert!(text.contains("Pick one?"), "{text}");
    assert!(text.contains("> 12. Option 12"), "{text}");
    assert!(!text.contains("Option 1 "), "{text}");
    assert!(!text.contains("Option 2"), "{text}");

    // The free-text row is reachable too once it scrolls into view.
    key(&mut state, KeyCode::Down);
    let text = render_panel(&state, 60, 12).join("\n");
    assert!(text.contains("13."), "{text}");
    assert!(text.contains('█'), "{text}");
}

/// With an overflowing option list, `n` draws the notes editor as an
/// overlay inside the panel instead of appending it below the fold where
/// it could never be seen.
#[test]
fn notes_editor_renders_as_overlay_when_options_overflow() {
    let options: Vec<_> = (1..=12).map(|i| option(&format!("Option {i}"))).collect();
    let mut state = state_with(vec![question("Pick one?", options)]);
    key(&mut state, KeyCode::Char('n'));
    assert!(state.editing_notes);

    let lines = render_panel(&state, 60, 12);
    let text = lines.join("\n");
    assert!(text.contains("Pick one?"), "{text}");
    assert!(text.contains('█'), "{text}");
    assert!(text.contains("Enter or Esc to save"), "{text}");
    // "Note:" sits inside its own bordered box (a `│` before the label).
    let note_row = lines
        .iter()
        .find(|line| line.contains("Note:"))
        .expect("notes overlay renders");
    assert!(
        note_row.trim_start().starts_with('│'),
        "notes editor should be a bordered overlay: {note_row:?}"
    );
}
