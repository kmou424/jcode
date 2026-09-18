# [20] Custom-model effort free input and swarm model override lock

## What it does

Two related changes around model/effort control:

1. **Custom-model effort free input.** Named OpenAI-compatible profiles
   (`[providers.<name>]`) serve user-configured models whose endpoints do not
   declare a fixed reasoning-effort ladder. For those providers,
   `/effort <level>` now accepts a typed canonical level
   (`none|minimal|low|medium|high|xhigh|max`, plus the `swarm`/`swarm-deep`
   UI sentinels) instead of reporting "not available". The provider runtime
   validates the typed value against
   `jcode_provider_core::canonical_reasoning_effort`, stores it, and forwards
   it verbatim as an OpenAI-style `reasoning_effort` request field so the
   endpoint decides how to apply it. Models on providers that *do* declare a
   ladder keep ladder-restricted selection: `/effort <level>` rejects values
   outside the advertised list with the list in the error, both locally and
   in remote sessions.

2. **`[agents] swarm_allow_override_model` (bool, default `true`).** When
   `false`:
   - the `swarm` tool schema omits the `model` and `effort` parameters
     entirely, and the tool description notes the lock;
   - dispatch defensively rejects any explicit `model` value (including
     `"inherit"`, since inheritance is already the default) with a tool
     error, and silently drops `effort`;
   - this covers every action that forwards worker model/effort: `spawn`,
     `assign_task`, `assign_next`, `fill_slots`, `run_plan` (all read the
     same sanitized `CommunicateInput`);
   - `[agents] swarm_model` / `swarm_effort` remain the pure-config way to
     steer worker model and effort; `list_models` drops its "pass effort"
     hint.

## Design notes

- Effort input surface: the typed `/effort <level>` command is the minimal
  surface, per the patch brief's allowed options — the model picker keeps a
  single plain row per custom model (a "custom effort…" pseudo-entry would
  need a new input box the picker does not have).
- "Custom model" is detected by provider name: `openai-compatible[:<id>]`
  in the TUI (`App::current_provider_is_custom_profile`), and
  `profile_id.is_some()` in the OpenRouter runtime
  (`accepts_custom_reasoning_effort`). Bare unnamed compat runtimes keep the
  old "not supported" rejection; remote `/effort` for non-custom providers
  with no declared ladder still forwards to the server for its own
  validation, unchanged.
- The swarm lock reads `crate::config::config().agents` at schema/dispatch
  time; no plumbing through `ToolContext` is needed.
- Config template documents `swarm_allow_override_model` in the `[agents]`
  section of the default config file.

## Config surface

- `[agents] swarm_allow_override_model = true|false` (default `true`).

## Affected files

- `crates/jcode-config-types/src/lib.rs` — new `AgentsConfig` field.
- `crates/jcode-base/src/config/default_file.rs` — template docs.
- `crates/jcode-app-core/src/tool/communicate.rs` — schema strip, dispatch
  rejection, description/list_models lock notes.
- `crates/jcode-provider-openrouter-runtime/src/lib.rs` —
  `accepts_custom_reasoning_effort`, request-field forwarding.
- `crates/jcode-provider-openrouter-runtime/src/openrouter_provider_impl.rs`
  — `reasoning_effort`/`set_reasoning_effort` custom path.
- `crates/jcode-tui/src/tui/app/model_context.rs` — custom-profile detection,
  local `/effort` bare output.
- `crates/jcode-tui/src/tui/app/remote/key_handling.rs` — remote `/effort`
  validation and free input.

## Upstream-fix candidacy

Both halves are candidates for upstream: the custom-profile effort path is a
general capability for OpenAI-compatible endpoints, and
`swarm_allow_override_model` is a general safety/lock knob. They are kept as
a local patch until PR'd upstream.
