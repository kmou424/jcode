# [27] Anthropic-compatible named provider profiles

## What it does

Named `[providers.<name>]` entries with
`type = "anthropic-compatible"` now build a fully isolated runtime for
that profile. Previously they inherited the stock
`AnthropicProvider::new()` path, so a bound profile silently fell back to
`api.anthropic.com`, `ANTHROPIC_API_KEY`, `anthropic.env`,
`auth.json`-backed Claude credentials, "Claude" labels, and Claude-only
retry/refresh behavior.

After this patch a bound profile:

- **never reads Claude env or auth files.** No `ANTHROPIC_API_KEY`,
  `anthropic.env`, `auth.json`, OAuth tokens, or `JCODE_ANTHROPIC_*`
  env overrides are consulted for URL, auth mode, headers, or
  credential.
- **talks only to the profile's `base_url`** (`<base>/messages`,
  `<base>/models`) with the profile's `auth` mode
  (`api-key`/`bearer`/`none`) and `headers`.
- **resolves the secret eagerly** via `api_key` / `api_key_env` /
  `api_key_cmd` (through `jcode_provider_env::resolve_secret_value`);
  the result is captured immutably in the binding, so env edits during a
  session cannot change the wire credential.
- **uses the profile's identity everywhere** — `display_name`,
  `runtime_display_name`, route keys, `/model` labels, session
  provider keys, and error strings name the profile (and its
  `display_name` when set), never "Claude".
- **skips Claude-only paths**: OAuth refresh, subscription model
  probing, "Claude credentials not available" errors, and
  Claude-default model fallback are all gated off when bound.

## Config

```toml
[providers.sub2api-anthropic]
type = "anthropic-compatible"
display_name = "Sub2API Claude"        # optional label override
base_url = "https://gateway.example/claude/v1"
auth = "bearer"                        # api-key | bearer | none
api_key_env = "SUB2API_KEY"            # or api_key / api_key_cmd
model = "claude-sonnet-4-6"
model_catalog = true                   # also fetch <base>/models
headers = { "x-tenant" = "alpha" }

[providers.sub2api-anthropic.models."claude-sonnet-4-6"]
display_name = "Claude Sonnet (sub2api)"
```

`model_catalog = true` enables a live `GET {base_url}/models` prefetch
that merges remote ids into the picker's model list for that profile.

## How it works

- `crates/jcode-provider-anthropic-runtime` exposes
  `AnthropicProfileBinding` (eager capture of name, display_name,
  base_url, auth, headers, resolved key, default model, declared models,
  catalog flag) plus `AnthropicProvider::new_for_named_profile` and the
  `DirectTransportConfig::from_binding` constructor. All Claude env
  lookups, OAuth paths, and profile-unaware fallbacks route through the
  binding instead.
- `jcode-base` adds `AnthropicRuntimeSpec::{Default, NamedProfile}` and a
  parameterized `anthropic_factory_slot()` factory; `ProviderRegistry`
  records the active binding in `anthropic_profile_binding` and rebinds
  on `fork()`, `set_model_on_named_provider_profile`, and remote model
  switches (`<profile>:<model>`).
- `ui_header` marks named anthropic-compatible profiles with the
  `api-key` credential tag.

## Affected files

- `crates/jcode-provider-anthropic-runtime/{Cargo.toml,src/lib.rs,src/anthropic_tests.rs}`
- `crates/jcode-base/src/provider/{external,mod,startup,accessors,dispatch}.rs`
- `crates/jcode-base/src/provider/tests*.rs` (struct-literal updates)
- `crates/jcode-tui/src/tui/ui_header.rs`
- `src/cli/startup.rs`
- `docs/NAMED_PROFILE_PROVIDER_IDENTITY.md` (spec)

## Upstream status

Upstream has no named-provider concept beyond openai-compatible; the
entire `AnthropicProfileBinding` + parameterized-factory path is
fork-only. The "error labels name the profile, not Claude" idea is a
good upstream candidate once upstream grows anthropic-compatible named
profiles.
