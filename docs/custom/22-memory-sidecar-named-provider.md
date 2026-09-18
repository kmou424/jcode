# [22] Memory sidecar named-provider specs

## What it does

`agents.memory_model` previously only accepted bare model ids that
`provider_for_model()` maps to `"openai"` or `"claude"` (routed to the
dedicated OpenAI/Claude HTTP backends). It now also accepts
`<providers.name>:<model>` specs, e.g.:

```toml
[agents]
memory_model = "sub2api:deepseek-v4.1-flash"
memory_reasoning_effort = "low"
```

so memory extraction/rerank can be pinned to any configured named
provider — independent of which provider the main session runs on.

## Behavior

- Bare OpenAI/Claude model ids are unchanged: they still take the
  dedicated HTTP backends (`SidecarBackend::OpenAI`/`Claude`).
- A `<prefix>:<model>` spec is accepted only when `prefix` is a key in
  `[providers]` config. The sidecar forks the live provider
  (`active_provider_fork()`), calls `fork.set_model(spec)`, and the fork
  binds that named profile's runtime (same machinery the model picker
  uses). The main session is undisturbed.
- Unknown prefixes, empty prefix/rest, missing active provider, and
  `set_model` failure all keep the historical warn +
  `auto_select_backend()` fallback — nothing silently routes to a wrong
  provider.
- `Sidecar.model` stores the **full spec** for this path
  (`model_name()` returns `sub2api:deepseek-v4.1-flash`). The field is
  display-only on the provider backend — the wire model lives in the
  fork — so keeping the prefix preserves the routing source in logs.
- `agents.memory_reasoning_effort` (new optional key): applied to the
  OpenAI backend via `reasoning_override` and to provider-routed specs
  via `fork.set_reasoning_effort()`; also applies to the auto-select
  OpenAI/Provider paths. The dedicated Claude path (haiku) has no
  reasoning surface and ignores it. Env override:
  `JCODE_MEMORY_REASONING_EFFORT`.

## Affected files

- `crates/jcode-base/src/sidecar.rs` — `with_configured_model` dispatches
  to `named_provider_or_auto`; `apply_configured_reasoning_effort` wires
  the effort key; tests cover spec binding + effort forwarding +
  unknown-prefix fallback.
- `crates/jcode-config-types/src/lib.rs` —
  `AgentsConfig.memory_reasoning_effort`.
- `crates/jcode-base/src/config/env_overrides.rs` —
  `JCODE_MEMORY_REASONING_EFFORT`.
- `crates/jcode-base/src/config/default_file.rs` — commented examples.

## Known limitations

- Auth-route prefixes (`openai-api:x`, `anthropic:x`) and catalog
  profiles are intentionally NOT accepted — only `[providers.<name>]`
  specs. Extending to those is a possible follow-up.
- `autoreview`/`autojudge` have no dedicated effort key (effort follows
  the model entry's `reasoning_effort` default) — separate known gap.
- `ambient.model`/`ambient.provider` are dead config (no consumer in
  the ambient runner) — separate known gap.

## Upstream status

Upstream-candidacy note: accepting named-provider specs for the memory
sidecar and a per-agent effort key are general features upstream could
want; the fallback path is unchanged so behavior is backward-compatible.
