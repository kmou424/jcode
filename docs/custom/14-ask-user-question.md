# [14] `ask_user_question` blocking interactive tool

## What it does

Adds an `ask_user_question` tool that lets the model pause its turn and ask
the user a structured questionnaire (1–4 questions, each with 1–4 authored
options plus a client-appended free-text row, optional multi-select and an
optional `(Recommended)` marker). The tool call blocks until the user answers
or cancels, then returns the answers rendered as text
(`"<header>: user selected: N. label"` / `"user wrote: …"`), matching the
format other agents (Codex/wow-pi) use for the same flow.

The tool is gated behind the experimentals tag `tool_ask_user_question`.

## Why

Some decisions genuinely belong to the user (approach choice, destructive
confirmation, picking between plausible plans). A blocking tool keeps the
question inside the tool-call lifecycle: the turn parks in
`SessionStatus::AwaitingUser`, the client renders the questionnaire, and the
answer lands as the tool result the model sees on resume.

## Wire and state surface

- `jcode-session-types::ask_user_question` — `AskUserQuestion`,
  `AskUserQuestionOption`, `AskUserQuestionAnswer`, `AskUserQuestionResult`,
  `PendingAskUserQuestion`; `validate_questions`, `normalize_questions`,
  `format_answers`. Shared by tool, protocol, session persistence, and TUI.
- `Request::AskUserQuestionResponse` / `ServerEvent::AskUserQuestion` —
  mirrors the stdin_request transport (question is re-presented on
  (re)attach while pending).
- `Session.pending_ask_user_question` (journal-persisted via
  `SessionJournalMeta`) — lets a resumed session re-present the question and
  lets a late answer be injected into history when the asking turn is gone.
- `Agent::ask_user_question_tx` + `ToolContext::ask_user_question_tx` —
  per-session channel, refreshed on (re)attach like `stdin_request_tx`.

## Server behavior

- `server::ask_user_question` keeps a session-scoped pending registry (one
  entry per session). A forwarder task registers the entry and emits
  `ServerEvent::AskUserQuestion`; it survives client disconnects so the
  blocked turn can be retained (`has_pending_ask_user_question` in the
  disconnect path).
- `handle_ask_user_question_response` resolves the live tool call on
  request-id match; otherwise `inject_orphaned_answer` writes the answer
  into session history as a synthetic `ToolResult` anchored to the recorded
  `tool_call_id` (or a user message when unknown) and clears the persisted
  pending record.
- Headless / `jcode run` (no client channel): the tool falls back to
  reading numbered-option lines on stdin; EOF/`cancel`/`quit` resolves the
  call as cancelled.
- Esc/cancel resolves the tool with `cancelled: true` and the tool result
  text `User cancelled the questionnaire`, matching stdin_request abort
  semantics.

## TUI surface

- Panel reworked to wow-pi parity (see `wow-pi` `ask-user-question`):
  horizontal question tabs + a Submit tab across the top, left option
  column + right markdown `preview` pane (side-by-side ≥100 cols, stacked
  below), `n` opens a per-question notes editor whose text merges into the
  answer's `notes`, and the composer is fully hidden while the panel is
  shown.
- `InlineAskUserQuestionState` + `InlineUiStateRef::AskUserQuestion` —
  takes precedence over picker/view states in `inline_ui_state()`. The
  state struct is a pure state machine (testable without `App`); the `App`
  shim only stages the finished result.
- `app/inline_ask_user_question.rs` — key routing: Tab/Shift+Tab/arrows
  switch tabs, Up/Down move the option row, Space toggles multi-select,
  Enter commits (or submits on the Submit tab), `n` edits notes, typing
  jumps to the free-text row, Esc cancels. Multi-select has no Done row:
  Enter is Done everywhere and commits the toggled set (a no-op while
  nothing is toggled); on the free-text row a typed draft commits as a
  Custom answer while an empty draft commits the toggled set.
- The free-text row is its own editor: selecting it with Up/Down (or
  landing on it after a tab switch) engages edit mode directly — no
  Enter-to-edit gate, matching the wow-pi inline editor. Leaving the row
  drops back to select mode while the draft survives; Esc still exits to
  select mode, then cancels.
- `ui_inline.rs::draw_ask_user_question` — full-width rounded panel with
  horizontal padding, tab bar / header chip, wrapped markdown question
  body, wrapped option list, markdown preview box (height-capped with a
  "N lines hidden" scissors row), notes editor block, key hints. Height =
  wrapped content + borders, capped at 60% window / 28 rows. Long text
  wraps via `jcode_tui_markdown::wrap_line` — no truncation.
- Notes + free-text editors are plain-OS fields: Left/Right/Home/End,
  Backspace char-delete, Alt+Backspace/Ctrl+W word-backward, Delete,
  typing inserts at cursor, Ctrl+U clears. Composer keybindings are not
  reused.
- Editor cursor: mid-text positions show the cell reversed; end-of-buffer
  shows a `█` glyph — not a reversed space, which the markdown wrapper
  drops as whitespace-only content (the cursor vanished on empty drafts).
  The free-text row keeps its `N.` number while editing.
- `respond_ask_user_question` sends the result inline for remote sessions
  (`RemoteConnection::send_ask_user_question_response`) and stages it in
  `pending_question_response` for local ones; `drain_pending_ask_user_question_responses`
  flushes the staged list after key handling and on the local input path.
- Divergences from wow-pi: no collapse key, no external editor, no
  PgUp/PgDn preview paging; single-line editors only. (The free-text row
  auto-edits on selection, same as wow-pi.)

## Affected files

- `crates/jcode-session-types/src/ask_user_question.rs` (new) + `lib.rs`,
  `Cargo.toml` (dev-dep `serde_json` for tests)
- `crates/jcode-tool-core/src/lib.rs` — `AskUserQuestionRequest` (session-scoped)
- `crates/jcode-protocol/src/{wire.rs,lib.rs}` — request/event variants,
  `Request::id()` arm
- `crates/jcode-base` — `Session.pending_ask_user_question`, journal meta
- `crates/jcode-app-core` — `tool/ask_user_question.rs` (new tool),
  `tool/mod.rs`, `experimentals/tool_ask_user_question.rs`,
  `server/ask_user_question.rs` (new), `server.rs`, `client_lifecycle.rs`,
  `client_session.rs`, `agent.rs`/`agent/status.rs`/`turn_execution.rs`
  (channel + `session()`/`session_mut()` accessors), `catchup.rs`
  (`AwaitingUser` arms), ToolContext field threading across test ctx literals
- `crates/jcode-tui` — `tui/mod.rs` (state + `InlineUiStateRef`),
  `tui/app.rs` + `app/impl_inline_ask_user_question.rs` (new) +
  `tui_lifecycle.rs` + `tui_state.rs`, `app/input.rs`, `app/remote.rs`,
  `app/remote/{server_events.rs,key_handling.rs}`, `backend.rs`,
  `ui_inline.rs`, `session_picker{,/render.rs}`, `workspace_client.rs`

## Upstream status

Candidate for upstream PR: the feature is generic (blocking structured
questions are a common agent UX — Codex's `request_user_input` and Claude
Code's `AskUserQuestion` are direct analogs). The session-status addition
(`AwaitingUser`) and the session-scoped pending registry are designed to be
upstream-friendly. Local-only parts are the experimentals gating and any
fork-specific TUI styling choices.
