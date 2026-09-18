# 11: Re-render transcript history live on display toggle changes

Patch: `[11] feat(tui): re-render history live on display toggle changes`

## Why

Changing a transcript display toggle used to apply only to messages rendered
*after* the change: already-rendered history stayed cached under the old
setting until something else (a new message, a resize) forced a rebuild.

Three independent prepared-render caches bake display settings into output
lines:

- `prepare_body_cached` (`tui/ui_prepare.rs`) keeps whole-transcript
  `PreparedMessages` keyed by `BodyCacheKey`, and reuses stale bases through
  `best_incremental_base` / `take_best_incremental_base` prefix reuse.
- `prepare_messages` keeps whole-frame `PreparedChatFrame`s keyed by
  `FullPrepCacheKey`.
- `get_cached_message_lines` (`jcode-tui-messages`) keeps per-message lines
  keyed by `MessageCacheKey`.

`show_bash_output`, `show_agentgrep_output`, `tool_call_details`, and
`reasoning_display` were absent from every key, so e.g.
`/tool-call-details on` left the body cache returning the exact same
`PreparedMessages` — the toggle visibly did nothing to history.
(`diff_mode` was already a direct key field on all three caches.)

## What changes

A process-wide **display epoch** now folds into all four cache keys
(`BodyCacheKey`, `FullPrepCacheKey`, `MessageCacheKey`, `AssistantAuxKey`).
`crate::tui::ui::display_epoch()` combines:

- `config_reload_generation()` — bumped on every config cache reload. All
  slash-command setters save the config file (which forces a reload), and
  direct `config.toml` edits or `JCODE_*` env changes reload on the next
  throttled check. This covers every `display.*` field through every
  mutation path — including toggles with no TUI command such as
  `show_bash_output` — with no per-setter hookups.
- `LOCAL_DISPLAY_EPOCH` — bumped by `crate::tui::ui::bump_display_epoch()`
  for in-scope mutations that do not write config (session-scoped
  `diff_mode` cycling) and by tests.

The incremental-base filters in `BodyCacheState` additionally require
`entry.key.display_epoch == key.display_epoch`, so a stale-epoch body can
never be reused as a rebuild prefix — without that, prefix reuse would
re-emit the stale rendered lines verbatim and silently defeat the whole
mechanism.

`MessageCacheContext` also gains the `reasoning_display` mode and sources
`show_bash_output`/`tool_call_details` from the same `tools_ui::*()`
accessors the renderers consult (honoring the thread-local test overrides),
so the key flips exactly when the rendered output would.

Toggle sites wired:

| Toggle | Sites |
| --- | --- |
| `diff_mode` | `/diff` (`apply_diff_mode`), diff-cycle keybinding (local + remote key handling), Alt+Shift+E expand badge. `app.diff_mode()` was already a key field; the bump is belt-and-braces and keeps the epoch contract honest. |
| `tool_call_details` | `/tool-call-details on\|off` → `Config::set_tool_call_details` (config save → generation bump) + explicit bump. |
| `show_agentgrep_output` | `/show-agentgrep-output on\|off` → `Config::set_show_agentgrep_output` + explicit bump. |
| `reasoning_display` | `/thinking-display\|/reasoning\|/thinking off\|full\|current` → `Config::set_reasoning_display` + explicit bump. |
| `show_bash_output` | No TUI command exists; covered by the config-generation fold (config.toml edit / env override → reload → epoch moves). |

Scroll position follows the normal rebuild path: tail-following stays
bottom-anchored (max_scroll recomputes from the rebuilt frame); a scrolled-up
offset is preserved numerically — same behavior as any other transcript
reflow. Remote (SSH) sessions render client-side through the same
`prepare_body_cached` path; `app/remote/` holds no parallel render cache, so
one fix covers both.

## Mode-agnostic reasoning rows (render-time gating)

Follow-up change on the same branch (`[11] feat(tui): mode-agnostic
reasoning display data + render-time mode gating`).

Reasoning display used to decide *at production time* what a thinking block
became: `off` dropped deltas entirely, `compact` collapsed the block into a
one-line `✻ thought for Ns` summary message, `current` sliced the block out
of the stream and anchored it as a live-only trace row. That made toggles
lossy — nothing was stored that a different mode could render, so the epoch
bump above re-rendered history with gaps.

Now a closed reasoning block always anchors a `role = "reasoning"`
`DisplayMessage` carrying the **full sentinel-wrapped markup plus the
measured `duration_secs`**, regardless of the mode that streamed it:

- `off` mode accumulates raw text into `App::reasoning_hidden_block`
  (shadow capture — nothing enters the live stream, no `reasoning_block_start`
  offset), and `close_reasoning_region` turns it into the same anchored row.
- `compact` keeps its live `✻ thinking…` header while streaming, but the
  header is stripped at close so the stored row holds only model text; the
  `✻ thought for N.Ns` label is *computed at render time* from
  `duration_secs` (identical semantics to patch 10: ceil to one decimal,
  `✻ thought` when missing).
- `current` behaves as before while live; the difference is that GC /
  `clear_turn_reasoning_traces` now only drop **live-turn membership**
  (`turn_reasoning_traces`), never the row. The row is permanent transcript
  data; membership is what `current` mode checks to decide "still live".

Visibility is decided at render time by the *current* mode via a single
accessor `crate::tui::ui::reasoning_display_mode()` (`ui_messages.rs`),
consulted by:

- `ui_prepare.rs` reasoning arm — `off` skips the row before the separator
  blank; `current` skips rows not registered in `turn_reasoning_traces`
  (`TuiState::reasoning_row_is_live`, default false).
- `render_reasoning_message` — `compact` projects the stored markup to the
  one-line label; `full`/`current` render it verbatim.
- `render_assistant_message` — strips residual sentinel runs from assistant
  content unless mode is `full` (covers pre-feature sessions, remote
  snapshots, mid-block toggles).
- `session_picker.rs` preview — `off`/`current` skip reasoning rows (picked
  sessions are always historical).

Supporting details:

- `take_streaming_text` auto-closes an open region (bounded recursion: the
  flag clears before re-entry) so turn-end commits can never strand live
  reasoning. Explicit `close_reasoning_region(None)` calls also precede the
  commits where `streaming_text` may already be empty (remote disconnect,
  server-events interrupt + turn finalize, local tool-execution boundary).
- `reasoning_close_duration_secs` is drained at the top of
  `close_reasoning_region` so a recorded duration binds to the block it
  measured even when the close arrives without a payload; fallbacks remain
  `thinking_start.elapsed()`.
- A mid-block mode toggle merges the hidden buffer and the streamed block in
  arrival order (`off` → visible lazily opens the stream block so its text is
  still sliced out at close; `compact` → anything still strips the live
  header).
- `rollback_streaming_attempt` accepts a trailing `assistant`+`reasoning`
  run, and `remove_display_message`/`replace_display_messages` keep trace
  indices coherent (shift vs clear).
- `session_picker.rs` preview — `off`/`current` skip reasoning rows (picked
  sessions are always historical).
- `DisplayMessage::reasoning_with_duration` exists alongside `reasoning`;
  `duration_secs` flows through `HistoryMessage`/`PreviewMessage`/`Rendered
  Message` (f64) into `DisplayMessage` (f32) at every conversion site.
- `#[cfg(test)] tests_reasoning_display_override` (thread-local, mirrors
  `tests_show_bash_output_override`) pins the render mode in tests that
  cannot use the `JCODE_HOME` helpers.

## Files

- `crates/jcode-tui/src/tui/ui.rs` — `LOCAL_DISPLAY_EPOCH`,
  `bump_display_epoch()`, `display_epoch()`, `display_epoch` fields on
  `BodyCacheKey`/`FullPrepCacheKey`, epoch check in all four
  incremental-base filters.
- `crates/jcode-tui/src/tui/ui_prepare.rs` — populate `display_epoch` in
  `FullPrepCacheKey`, `BodyCacheKey`, and `AssistantAuxKey`.
- `crates/jcode-tui-messages/src/cache.rs` — `reasoning_display` +
  `display_epoch` on `MessageCacheContext`/`MessageCacheKey`.
- `crates/jcode-tui/src/tui/ui_messages_cache.rs` — populate the new
  context fields via render-source accessors; epoch-bump cache test.
- `crates/jcode-tui/src/tui/app/commands.rs` — epoch bumps in
  `apply_diff_mode`, `/tool-call-details`, `/show-agentgrep-output`,
  `/thinking-display`.
- `crates/jcode-tui/src/tui/app/input.rs` — bumps on the local diff-cycle
  keybinding and the Alt+Shift+E FullInline path.
- `crates/jcode-tui/src/tui/app/remote/key_handling.rs` — bump on the
  remote diff-cycle keybinding.
- `crates/jcode-tui/src/tui/ui_tests/basic/body_cache.rs` — new-epoch field
  on all key literals; regression tests: stale-epoch bodies must neither
  exact-hit nor serve as incremental bases (regular + oversized lanes),
  full-prep cache must not hit across epochs, epoch moves on local bump.
- `crates/jcode-tui/src/tui/ui_tests/prepare.rs` — render-path test: the
  same historical bash tool message re-renders with output after the
  `show_bash_output` toggle flips.

## Upstream status

General-purpose fix; a cleaned-up version is a plausible upstream PR. The
`config_reload_generation()` fold also repairs adjacent latent staleness for
other `display.*` fields not enumerated here (e.g. `markdown_spacing`,
`emoji`) on config hot-reload.

This commit also carries an upstream test-infrastructure fix that blocked
this patch's suite validation (allowed under the "blocks the feature being
landed" rule): `test_alt_shift_i_toggles_inline_images_and_persists`
acquired `scroll_render_test_lock()` before `lock_test_env()`, while
env-using tests take them in the opposite order — a deterministic ABBA
deadlock whenever they are co-scheduled (reproduced on baseline upstream).
The two guard acquisitions were swapped. Candidate for an upstream PR.
