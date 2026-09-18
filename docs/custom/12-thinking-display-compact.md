# [12] Compact (Claude Code style) thinking display mode

## What it does

Adds a fourth `ReasoningDisplayMode` variant, `compact`, that mirrors Claude
Code's thinking UX:

- **While thinking streams**, the block opens with a live `✻ thinking…` header
  line followed by the reasoning text in the usual dim+italic markup.
- **When the block closes**, the whole block (header + body) collapses in
  place to a single dim+italic `✻ thought for N.Ns` trace (one decimal, rounded up),
  anchored exactly where it streamed. Like `current`, the trace is ephemeral:
  it is never persisted to the transcript and is cleared when the next user
  prompt starts a new turn.
- If no answer text precedes the block the trace anchors at the same display
  position the block occupied; preceding answer text commits first so
  chronological order is preserved.

Config: `display.reasoning_display = "compact"` (aliases `claude`, `cc` are
also accepted). `/thinking-display` cycles `off → current → compact → full →
off` and shows `Compact` in the picker. The `current` mode's output is
unchanged.

## Design notes

- `ReasoningDisplayMode::Compact` lives in `jcode-config-types`; parsing,
  serde round-trip, `label()`, and `next()` cycling are covered there.
- `reasoning_region.rs` gained `COMPACT_REASONING_LIVE_HEADER`,
  `compact_reasoning_summary_label` (`✻ thought for N.Ns`, one decimal, ceil),
  `reasoning_compact_mode()`, and `anchor_compact_reasoning_block()` — the
  compact twin of `anchor_current_reasoning_block` that discards the block
  and anchors only the summary line (wrapped in the reasoning sentinel
  markup so it renders dim+italic and commits can strip it).
- **Duration pairing.** The paced `StreamBuffer` can hold a region's final
  reasoning characters (and its close marker) long after the provider's
  `ThinkingDone`/`ReasoningDone` arrives, so the duration cannot live only
  in `App` state: a later block's events could overwrite it before the close
  lands, and the buffered open could clobber a just-recorded value. The fix
  is `StreamOp::CloseReasoning { duration_ms: Option<u64> }` — the measured
  duration rides inside the close marker through the paced queue, keeping
  each duration paired with its own block (`u64` ms keeps `StreamOp: Eq`).
  Priority at collapse time: close-op payload → `reasoning_close_duration_secs`
  (recorded at `ThinkingEnd`/`ThinkingDone`/`ReasoningDone`, drained at every
  close) → `thinking_start` elapsed → `0s`.
- History replay (`jcode-base` render) hides persisted reasoning under
  `compact` exactly like `current` — the trace is a live-only artifact; no
  duration is persisted.

## TDD evidence

- `jcode-config-types`: `compact_reasoning_display_parse_label_and_cycle`,
  `compact_reasoning_display_round_trips_through_config_serde` (watched fail
  before the enum variant existed).
- `jcode-tui` (`tests/reasoning_region.rs`): live `✻ thinking…` header +
  dim/italic body while streaming; one-line `✻ thought for N.Ns` collapse with
  floor seconds; `thinking_start` elapsed fallback; ordering after preceding
  answer text; sentinel stripping on commit; trace clearing on next prompt;
  remote `ReasoningDone`-reported duration winning over wall clock; and a
  rendered-line assertion that the collapsed trace is exactly one italic
  line. Three tests initially failed with `0s`, which exposed the
  paced-buffer pairing bug and motivated the `duration_ms` op payload.
- `jcode-tui-core` `stream_buffer` tests updated for the new op payload; all
  17 pass.

## Affected files

- `crates/jcode-config-types/src/lib.rs` — `Compact` variant, label, parse
  aliases, cycle order, tests.
- `crates/jcode-tui-core/src/stream_buffer.rs` — `duration_ms` payload on
  `QueuedOp`/`StreamOp::CloseReasoning`, `push_close_reasoning(Option<f64>)`.
- `crates/jcode-tui/src/tui/app/input/reasoning_region.rs` — live header,
  collapse-to-summary, duration plumbing, `reasoning_compact_mode()`.
- `crates/jcode-tui/src/tui/app.rs`, `tui_lifecycle.rs` —
  `reasoning_close_duration_secs` field + ctor inits.
- `crates/jcode-tui/src/tui/app/turn.rs` — `ThinkingEnd` records elapsed,
  `ThinkingDone` records provider duration and bundles it into the close op.
- `crates/jcode-tui/src/tui/app/remote/server_events.rs` — `ReasoningDone`
  resolves reported-or-elapsed duration into the close op.
- `crates/jcode-tui/src/tui/app/input.rs` — `apply_stream_ops` forwards the
  op payload to `close_reasoning_region`; `collapse_reasoning_for_commit`
  covers `Compact`.
- `crates/jcode-base/src/session/render.rs` — replay hides reasoning under
  `compact`.
- `crates/jcode-tui/src/tui/app/commands.rs`,
  `state_ui_input_helpers.rs`, `display.rs`, `default_file.rs`,
  `tests/reasoning_region.rs`, `tests/support_failover/part_01.rs` —
  help/usage text, `/thinking-display` description, config template, field
  docs, tests, and the `with_reasoning_compact_home` helper.

## Upstream status

**Upstream-fix candidate.** The feature is generic (new enum variant + a
self-contained display path) and could be PR'd upstream; the
`StreamOp::CloseReasoning` payload change is the only part that touches a
shared reveal primitive, and it is additive.
