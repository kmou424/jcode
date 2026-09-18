# [17] Model identity in the system prompt + switch notification

## What it does

Injects a model-identity line into the `## Identity` section of the system
prompt — "You are jcode, an agent powered by
`provider/model(display_name)`." (e.g.
`openai/gpt-6-astra(GPT-6 Astra)`) — and appends a model-visible
`<system-reminder>` history note whenever the model switches
("Model switched: previous → next").

## Why

The built-in prompt previously never told the model what it was
(`model_display_name()` existed for UI but never reached the prompt), so a
model could not report its own identity and a mid-session switch was
invisible to the model itself. The deepseek harness does this with a
`{{model}}` template variable re-interpolated at assembly time; jcode's
system prompt is already rebuilt every turn
(`build_system_prompt_split`), so injecting at build time is nearly free
and follows the same pattern.

## Design

- `system_prompt.md` gains a `{{MODEL_IDENTITY}}` placeholder line inside
  `## Identity`. `apply_model_identity()` in `jcode-base/src/prompt.rs`
  substitutes it with the rendered line; when the base prompt is a
  user-supplied file without the placeholder, a `# Model Identity`
  section is appended instead; with no identity available the placeholder
  line is removed.
- `provider_route_label()` → `provider/model` (including an explicit
  `model@pin`); `model_identity_label(provider, provider_key)` wraps it
  as `provider/model(display_name)` when the named-provider config gives
  the model a `display_name`, else the bare route.
- `agent/provider.rs` records the label at every model/route mutation;
  `server/provider_control.rs` diffs old vs new on switches and calls
  `Session::append_model_switch_notice`, which writes the reminder as a
  model-visible but transcript-hidden user message (same mechanism class
  as other system notes) so it persists in session history across resume.
- `tui/app/turn_memory.rs` feeds the identity into the prompt-build path.

## Config surface

None — uses the existing `[providers.<profile>]` `[[models]]`
`display_name` field ([01]) and the persisted `provider_key`.

## Affected files

- `crates/jcode-base/src/prompt.rs`, `prompt/system_prompt.md`,
  `prompt_tests.rs`
- `crates/jcode-base/src/provider/mod.rs` (route/identity labels)
- `crates/jcode-base/src/session.rs` + `session_tests/cases.rs`
  (switch notice)
- `crates/jcode-app-core/src/agent/prompting.rs`, `agent/provider.rs`,
  `server/provider_control.rs`
- `crates/jcode-tui/src/tui/app/turn_memory.rs`

## Upstream status

Upstream candidate (identity line + switch notice are generic). Folds the
same upstream `duration_secs` test-literal fix as [12]/[16]
(upstream-fix candidacy).
