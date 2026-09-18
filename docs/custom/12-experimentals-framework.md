# [12] Per-model `experimentals` tool-gating framework

## What it does

Adds an `experimentals = [...]` string-array field to `[[models]]` entries
under `[providers.<profile>]` in `config.toml`. Each tag names an
experimental feature; the framework reconciles the *session's live tool
map* against the active model's tag set once per turn, so a model switch
changes the advertised tool surface on the next request.

```toml
[providers.myprofile]
type = "openai-compatible"
# ...
  [[providers.myprofile.models]]
  id = "some-model"
  experimentals = ["tool_apply_patch", "tool_ask_user_question"]
```

## Why

Experimental tools should be opt-in per model (a GPT-distilled model may
need different tool variants than a frontier model) and must never be a
build-time or session-start decision: the same session can switch models
mid-conversation, and the tool surface must follow. Per-session mutation
of the registry's live map (rather than filtering definitions at
serialization) also means execution dispatch sees exactly what was
advertised.

## Design

- `crates/jcode-app-core/src/experimentals.rs` is the dispatcher. Each
  feature lives in a submodule exporting `pub const TAG` + `apply(tools)`;
  the dispatcher's `KNOWN_TAGS` list and `match` import the constants so a
  renamed/removed feature fails at compile time. Hooks run in
  `KNOWN_TAGS` order (a later hook wins a shared tool name); the transform
  is idempotent and reports whether the map changed.
- `Agent` calls `Registry::apply_experimentals(&active_experimentals())`
  in `turn_execution` before consulting the locked tool-definition
  snapshot; a changed map invalidates the cache (one intentional miss,
  same trade as the late-MCP rebuild).
- `active_experimentals()` resolves tags via
  `named_provider_model_experimentals_for_provider_key(provider_key,
  model)` in `jcode-base/src/provider_catalog.rs` — the persisted
  `openai-compatible:<profile>` provider key locates the profile, with a
  `JCODE_NAMED_PROVIDER_PROFILE` env fallback for the active route.
  Non-named routes resolve to an empty set.
- Unknown tags are ignored with a once-per-process warning so newer
  configs stay loadable on older builds.
- `apply_patch` is removed from the tool map unless an apply_patch-family
  tag is set (see [13]); `tool_ask_user_question` is a recognized no-op
  until [14] registers the tool.

## Config surface

- `NamedProviderModelConfig.experimentals: Vec<String>`
  (`jcode-config-types`), `#[serde(default, skip_serializing_if)]`.

## Affected files

- `crates/jcode-app-core/src/experimentals.rs` +
  `experimentals/tool_apply_patch{,_compat}.rs`,
  `experimentals/tool_ask_user_question.rs` (new)
- `crates/jcode-app-core/src/agent/turn_execution.rs` (apply call,
  `active_experimentals`, prewarm sync)
- `crates/jcode-app-core/src/tool/mod.rs` (`apply_experimentals`)
- `crates/jcode-base/src/provider_catalog.rs` (tag resolution helpers)
- `crates/jcode-config-types/src/lib.rs` (config field)
- `crates/jcode-base/src/config_tests.rs`,
  `crates/jcode-provider-openrouter-runtime` tests,
  `src/cli/commands/provider_setup.rs` (struct-literal updates)

## Upstream status

Upstream candidate (generic per-model tool gating). Also folds the
upstream `duration_secs` test-literal fix needed to compile app-core test
targets on v0.86.0 (upstream-fix candidacy).
