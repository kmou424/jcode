//! Blocking `ask_user_question` questionnaire.
//!
//! The panel renders above the transcript while the tool's turn is parked
//! awaiting answers; the normal composer is hidden for the duration. The
//! state machine lives on `InlineAskUserQuestionState` (tabs across the top,
//! a submit tab, per-question notes merged into answers) and this module's
//! `App` shim only stages the finished `AskUserQuestionResult` for the
//! remote drain to post as `AskUserQuestionResponse`. Esc cancels the whole
//! questionnaire, matching the stdin_request abort semantics.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use jcode_session_types::{AskUserQuestionAnswer, AskUserQuestionResult, AskUserQuestionSelection};

use super::App;
use crate::tui::{AskUserQuestionOutcome, AskUserQuestionRow, InlineAskUserQuestionState};

impl App {
    /// Open the questionnaire for a server-originated pending request.
    pub(super) fn open_ask_user_question(
        &mut self,
        request_id: String,
        questions: Vec<jcode_session_types::AskUserQuestion>,
    ) {
        if questions.is_empty() {
            return;
        }
        self.inline_ask_user_question_state =
            Some(InlineAskUserQuestionState::new(request_id, questions));
    }

    /// Whether the questionnaire currently owns key input.
    pub(super) fn has_ask_user_question(&self) -> bool {
        self.inline_ask_user_question_state.is_some()
    }

    /// Route one key into the questionnaire. Returns `Ok(true)` when the key
    /// was consumed (always, while the panel is open) so callers can stop
    /// further input handling.
    pub(super) fn handle_ask_user_question_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<bool> {
        let Some(state) = self.inline_ask_user_question_state.as_mut() else {
            return Ok(false);
        };
        if let AskUserQuestionOutcome::Finish(result) = state.handle_key(code, modifiers) {
            let request_id = state.request_id.clone();
            self.pending_ask_user_question_response = Some((request_id, result));
            self.inline_ask_user_question_state = None;
        }
        Ok(true)
    }
}

impl InlineAskUserQuestionState {
    /// Route one key through the questionnaire state machine: notes editor,
    /// free-text editor, submit tab, then the select-mode bindings.
    pub(crate) fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> AskUserQuestionOutcome {
        if self.editing_notes {
            self.handle_notes_key(code, modifiers);
            return AskUserQuestionOutcome::Stay;
        }
        if self.editing_custom {
            return self.handle_custom_key(code, modifiers);
        }
        if self.on_submit_tab() {
            return self.handle_submit_tab_key(code, modifiers);
        }
        self.handle_select_key(code, modifiers)
    }

    /// Select-mode bindings on a question tab.
    fn handle_select_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> AskUserQuestionOutcome {
        // Tab switching wraps across the question tabs and the submit tab.
        if self.is_multi() {
            let total = self.questions.len() + 1;
            match code {
                KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                    self.switch_tab(wrap_index(self.question_index as isize + 1, total));
                    return AskUserQuestionOutcome::Stay;
                }
                KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                    self.switch_tab(wrap_index(self.question_index as isize - 1, total));
                    return AskUserQuestionOutcome::Stay;
                }
                _ => {}
            }
        }

        match code {
            KeyCode::Esc => return self.finish(true),
            KeyCode::Up | KeyCode::Char('k') => self.nav_rows(-1),
            KeyCode::Down | KeyCode::Char('j') => self.nav_rows(1),
            KeyCode::Char('n') if !modifiers.contains(KeyModifiers::CONTROL) => self.open_notes(),
            KeyCode::Enter => {
                // Multi-select has no Done row: Enter itself is Done and
                // commits the toggled set (a no-op while nothing is
                // toggled). Free text stays reachable by typing, which
                // opens the editor directly.
                let multi = self
                    .current_question()
                    .is_some_and(|question| question.is_multi_select());
                match self.row_kind(self.row_index) {
                    AskUserQuestionRow::Option if multi => return self.commit_multi(),
                    AskUserQuestionRow::Option => return self.commit_option(self.row_index),
                    AskUserQuestionRow::Custom if multi => return self.commit_multi(),
                    AskUserQuestionRow::Custom => self.enter_custom_editing(),
                }
            }
            KeyCode::Char(' ') => match self.row_kind(self.row_index) {
                AskUserQuestionRow::Option => {
                    if self
                        .current_question()
                        .is_some_and(|question| question.is_multi_select())
                    {
                        self.toggle_option(self.row_index);
                    } else {
                        return self.commit_option(self.row_index);
                    }
                }
                AskUserQuestionRow::Custom => self.enter_custom_editing(),
            },
            KeyCode::Backspace => {
                if self.row_kind(self.row_index) == AskUserQuestionRow::Custom {
                    delete_char_before(&mut self.custom_text, &mut self.custom_cursor);
                }
            }
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
                // Typing anywhere jumps to the free-text row and starts
                // editing, so a user can answer with prose immediately.
                self.row_index = self
                    .current_question()
                    .map(|question| question.options.len())
                    .unwrap_or(0);
                self.editing_custom = true;
                insert_char(&mut self.custom_text, &mut self.custom_cursor, c);
            }
            _ => {}
        }
        AskUserQuestionOutcome::Stay
    }

    /// Submit tab bindings: up/down pick Submit or Cancel, Enter confirms,
    /// tab keys still move between tabs, Esc cancels.
    fn handle_submit_tab_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AskUserQuestionOutcome {
        let total = self.questions.len() + 1;
        match code {
            KeyCode::Esc => self.finish(true),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.switch_tab(wrap_index(self.question_index as isize + 1, total));
                AskUserQuestionOutcome::Stay
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                self.switch_tab(wrap_index(self.question_index as isize - 1, total));
                AskUserQuestionOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.submit_choice = 0;
                AskUserQuestionOutcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.submit_choice = 1;
                AskUserQuestionOutcome::Stay
            }
            KeyCode::Enter => self.finish(self.submit_choice == 1),
            _ => AskUserQuestionOutcome::Stay,
        }
    }

    /// Free-text editor keys: Enter submits a non-empty draft, Esc exits and
    /// discards the draft, everything else is plain text editing.
    fn handle_custom_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> AskUserQuestionOutcome {
        match code {
            KeyCode::Esc => {
                self.editing_custom = false;
                self.custom_text.clear();
                self.custom_cursor = 0;
            }
            KeyCode::Enter => {
                let text = self.custom_text.trim().to_string();
                if text.is_empty() {
                    // Multi-select keeps Enter as Done even on the free-text
                    // row: an empty draft commits the toggled set instead of
                    // doing nothing.
                    if self
                        .current_question()
                        .is_some_and(|question| question.is_multi_select())
                    {
                        self.editing_custom = false;
                        return self.commit_multi();
                    }
                    return AskUserQuestionOutcome::Stay;
                }
                let tab = self.question_index;
                self.answers[tab] = Some(AskUserQuestionAnswer::Custom {
                    question_index: tab,
                    text,
                    notes: self.note_for(tab).map(str::to_string),
                });
                self.editing_custom = false;
                self.custom_text.clear();
                self.custom_cursor = 0;
                return self.advance();
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.custom_text.clear();
                self.custom_cursor = 0;
            }
            KeyCode::Up => self.nav_rows(-1),
            KeyCode::Down => self.nav_rows(1),
            _ => {
                edit_buffer(
                    &mut self.custom_text,
                    &mut self.custom_cursor,
                    code,
                    modifiers,
                );
            }
        }
        AskUserQuestionOutcome::Stay
    }

    /// Notes editor keys: Enter or Esc saves the note into the side-band;
    /// every other key is plain text editing on the note draft.
    fn handle_notes_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Enter | KeyCode::Esc => self.commit_notes(),
            _ => {
                edit_buffer(
                    &mut self.notes_text,
                    &mut self.notes_cursor,
                    code,
                    modifiers,
                );
            }
        }
    }

    /// Move the row cursor within the current question, wrapping.
    fn nav_rows(&mut self, delta: isize) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        self.row_index = wrap_index(self.row_index as isize + delta, count);
        self.sync_editing_to_row();
    }

    /// The free-text row is its own editor: selecting it engages editing
    /// directly rather than requiring an Enter to switch into an edit view
    /// (wow-pi parity). Leaving the row drops back to select mode while the
    /// draft survives in `custom_text`.
    fn sync_editing_to_row(&mut self) {
        if self.row_kind(self.row_index) == AskUserQuestionRow::Custom {
            self.enter_custom_editing();
        } else {
            self.editing_custom = false;
        }
    }

    /// Switch to a different tab, restoring the custom draft a committed
    /// Custom answer carried and reseeding the notes draft.
    fn switch_tab(&mut self, next: usize) {
        if next == self.question_index || next > self.questions.len() {
            return;
        }
        self.question_index = next;
        self.row_index = 0;
        self.editing_custom = false;
        self.editing_notes = false;
        self.submit_choice = 0;
        self.custom_text = match self.answers.get(next) {
            Some(Some(AskUserQuestionAnswer::Custom { text, .. })) => text.clone(),
            _ => String::new(),
        };
        self.custom_cursor = char_count(&self.custom_text);
        // A question with zero options lands on the free-text row, which is
        // its own editor.
        self.sync_editing_to_row();
    }

    /// Open the notes editor for the current question tab.
    fn open_notes(&mut self) {
        if self.on_submit_tab() {
            return;
        }
        let tab = self.question_index;
        self.notes_text = self.note_for(tab).unwrap_or_default().to_string();
        self.notes_cursor = char_count(&self.notes_text);
        self.editing_notes = true;
    }

    /// Commit the notes draft into the side-band (trimmed; empty deletes)
    /// and mirror it onto an already-committed answer so editing a note
    /// after answering still lands.
    fn commit_notes(&mut self) {
        let tab = self.question_index;
        let trimmed = self.notes_text.trim().to_string();
        if trimmed.is_empty() {
            if let Some(slot) = self.notes_by_question.get_mut(tab) {
                *slot = None;
            }
        } else if let Some(slot) = self.notes_by_question.get_mut(tab) {
            *slot = Some(trimmed.clone());
        }
        if let Some(Some(answer)) = self.answers.get_mut(tab) {
            set_answer_notes(
                answer,
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                },
            );
        }
        self.editing_notes = false;
        self.notes_text.clear();
        self.notes_cursor = 0;
    }

    /// Put the free-text row into edit mode, seeding the draft from a
    /// committed Custom answer when the buffer is empty.
    fn enter_custom_editing(&mut self) {
        if self.custom_text.is_empty()
            && let Some(Some(AskUserQuestionAnswer::Custom { text, .. })) =
                self.answers.get(self.question_index)
        {
            self.custom_text = text.clone();
        }
        self.custom_cursor = char_count(&self.custom_text);
        self.editing_custom = true;
    }

    /// Toggle a multi-select option and persist the checked set as the
    /// question's answer so the submit-tab summary stays live.
    fn toggle_option(&mut self, index: usize) {
        let tab = self.question_index;
        if let Some(selected) = self.multi_selected.get_mut(tab) {
            if !selected.remove(&index) {
                selected.insert(index);
            }
        }
        self.persist_multi();
    }

    /// Persist the current checked set as the multi answer for this tab.
    fn persist_multi(&mut self) {
        let tab = self.question_index;
        let Some(question) = self.current_question() else {
            return;
        };
        let selected: Vec<AskUserQuestionSelection> = self
            .multi_selected
            .get(tab)
            .into_iter()
            .flatten()
            .filter_map(|index| {
                question
                    .options
                    .get(*index)
                    .map(|option| AskUserQuestionSelection {
                        index: *index,
                        label: stamped_option_label(option),
                    })
            })
            .collect();
        if selected.is_empty() {
            self.answers[tab] = None;
        } else {
            self.answers[tab] = Some(AskUserQuestionAnswer::Multi {
                question_index: tab,
                selected,
                notes: self.note_for(tab).map(str::to_string),
            });
        }
    }

    /// Commit a single-choice option answer and advance.
    fn commit_option(&mut self, index: usize) -> AskUserQuestionOutcome {
        let tab = self.question_index;
        let Some(question) = self.current_question() else {
            return AskUserQuestionOutcome::Stay;
        };
        let Some(option) = question.options.get(index) else {
            return AskUserQuestionOutcome::Stay;
        };
        self.answers[tab] = Some(AskUserQuestionAnswer::Option {
            question_index: tab,
            index,
            label: stamped_option_label(option),
            notes: self.note_for(tab).map(str::to_string),
        });
        self.advance()
    }

    /// Commit the toggled set on a multi-select question: Enter is the
    /// Done gesture now that there is no separate Done row. With nothing
    /// toggled the press is a no-op so the user cannot silently skip.
    fn commit_multi(&mut self) -> AskUserQuestionOutcome {
        if self
            .multi_selected
            .get(self.question_index)
            .is_none_or(|set| set.is_empty())
        {
            return AskUserQuestionOutcome::Stay;
        }
        self.persist_multi();
        self.advance()
    }

    /// Move to the next question or the submit tab; a single-question
    /// questionnaire finishes immediately on answer.
    fn advance(&mut self) -> AskUserQuestionOutcome {
        if !self.is_multi() {
            return self.finish(false);
        }
        self.switch_tab(self.question_index + 1);
        AskUserQuestionOutcome::Stay
    }

    /// Close the questionnaire and return the wire result.
    fn finish(&mut self, cancelled: bool) -> AskUserQuestionOutcome {
        let answers = if cancelled {
            Vec::new()
        } else {
            self.answers.iter().flatten().cloned().collect()
        };
        AskUserQuestionOutcome::Finish(AskUserQuestionResult { cancelled, answers })
    }
}

/// Label stamped onto a committed answer: the client-painted
/// `(Recommended)` suffix travels with the label.
fn stamped_option_label(option: &jcode_session_types::AskUserQuestionOption) -> String {
    if option.recommended == Some(true) {
        format!(
            "{}{}",
            option.label,
            jcode_session_types::RECOMMENDED_SUFFIX
        )
    } else {
        option.label.clone()
    }
}

/// Set or strip the `notes` field on a committed answer.
fn set_answer_notes(answer: &mut AskUserQuestionAnswer, notes: Option<String>) {
    match answer {
        AskUserQuestionAnswer::Option { notes: slot, .. }
        | AskUserQuestionAnswer::Multi { notes: slot, .. }
        | AskUserQuestionAnswer::Custom { notes: slot, .. } => *slot = notes,
    }
}

/// Wrap an index into `[0, total)`.
fn wrap_index(index: isize, total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    let total = total as isize;
    ((index % total) + total) as usize % total as usize
}

fn char_count(text: &str) -> usize {
    text.chars().count()
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn insert_char(text: &mut String, cursor: &mut usize, c: char) {
    let byte = char_to_byte(text, *cursor);
    text.insert(byte, c);
    *cursor += 1;
}

fn delete_char_before(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = char_to_byte(text, *cursor - 1);
    let end = char_to_byte(text, *cursor);
    text.replace_range(start..end, "");
    *cursor -= 1;
}

fn delete_char_at(text: &mut String, cursor: usize) {
    if cursor >= char_count(text) {
        return;
    }
    let start = char_to_byte(text, cursor);
    let end = char_to_byte(text, cursor + 1);
    text.replace_range(start..end, "");
}

/// Delete the whitespace run and the word before the cursor (Alt+Backspace
/// / Ctrl+W word-backward semantics).
fn delete_word_before(text: &mut String, cursor: &mut usize) {
    let chars: Vec<char> = text.chars().collect();
    let mut start = (*cursor).min(chars.len());
    while start > 0 && chars[start - 1].is_whitespace() {
        start -= 1;
    }
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let byte_start = char_to_byte(text, start);
    let byte_end = char_to_byte(text, *cursor);
    text.replace_range(byte_start..byte_end, "");
    *cursor = start;
}

/// Plain-OS text-field editing shared by the free-text and notes drafts:
/// cursor left/right, Home/End edges, Backspace char delete, Alt+Backspace
/// or Ctrl+W word-backward, Delete forward, printable chars insert at the
/// cursor.
fn edit_buffer(text: &mut String, cursor: &mut usize, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Left => *cursor = cursor.saturating_sub(1),
        KeyCode::Right => *cursor = (*cursor + 1).min(char_count(text)),
        KeyCode::Home => *cursor = 0,
        KeyCode::End => *cursor = char_count(text),
        KeyCode::Backspace => {
            if modifiers.contains(KeyModifiers::ALT) || modifiers.contains(KeyModifiers::CONTROL) {
                delete_word_before(text, cursor);
            } else {
                delete_char_before(text, cursor);
            }
        }
        KeyCode::Delete => delete_char_at(text, *cursor),
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            text.clear();
            *cursor = 0;
        }
        KeyCode::Char(c)
            if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
        {
            insert_char(text, cursor, c);
        }
        _ => {}
    }
}
