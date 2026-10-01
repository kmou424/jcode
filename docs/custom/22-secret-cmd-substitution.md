# [22] `!{cmd}` substitution for secret config values

## What it does

Secret-typed config values may now hold a whole-value `!{command}` expression
instead of the secret itself, e.g.:

```toml
[safety]
email_password = "!{pass show jcode/smtp}"
jade_relay_token = "!{op read op://vault/jade/token}"

[providers.myprofile]
api_key = "!{bw get item 'anthropic-key' | jq -r .notes}"
```

The command runs via `sh -c` (stdin null, 10s timeout, stdout trimmed) and its
stdout becomes the value. The `!{cmd}` literal is never used as a credential:
a failed, timed-out, or empty substitution collapses to `None`, so the
consumer reports "not configured" and the log carries the reason (stderr +
status are captured and warned).

## Surface

- `jcode-provider-env` owns the shared machinery (`src/substitution.rs`):
  - `expand_environment_string` — moved verbatim from `mcp/protocol.rs`; MCP
    delegates, so inline `!{}` semantics (failed command leaves the literal)
    are unchanged.
  - `resolve_secret_value` — the secret contract: the *whole* trimmed value
    must be `!{...}`; mixed strings like `Bearer !{x}` are passed through
    untouched.
  - Results are memoized per command for the process lifetime (success and
    failure alike — a broken keychain helper does not respawn per lookup).
    `Config::save` → `invalidate_config_cache` clears the cache, so saving
    config refreshes secrets without a restart.
- Coverage at `clean_loaded_value`: every `KEY=value` line in env files and
  every process env var read through the provider-env loaders gets whole-value
  `!{}` support in one place — that includes `api_key_env`-indirect keys.
- Inline `api_key = "!{cmd}"` in a named provider profile is resolved at
  `provider_catalog` apply time (before `set_var`) so the generated env var
  always carries the resolved secret — required because
  `provider/anthropic.rs` reads the indirection env var raw. `!{` values skip
  the inline-key leak warning (they store no secret).
- The same inline `api_key` is also resolved at read time inside
  `new_named_openai_compatible` (`resolve_inline_api_key`) — the
  openai-compatible runtime reads `profile.api_key` directly for its Bearer
  token, bypassing catalog apply entirely; without this the literal
  `!{...}` string went out as the credential.
- TOML secret fields resolved at read: `email_password`, `telegram_bot_token`,
  `discord_bot_token`, `jade_relay_token`/`_id`, `websearch.bing_api_key` —
  including the `bing_api_key_env` env-var path.
- Write-back safety: `config.save()` re-serializes the raw `!{cmd}` string
  (resolution is read-side only); `upsert_env_file_value` rewrites only its
  own `KEY=` line, leaving sibling `!{}` lines verbatim.

## Caveats

- Keychain CLIs that block on a TTY (e.g. `bw` unlock prompts) will hit the
  10s timeout in daemon contexts — pre-unlock or use non-interactive auth.
- The same `!{}` value in two fields shares one memoized execution.
- Secret rotation requires `Config::save` or a process restart to re-run.

## Files

- `crates/jcode-provider-env/src/substitution.rs` (new) — substitution engine,
  `resolve_secret_value`, memo, tests.
- `crates/jcode-provider-env/src/lib.rs` — module export; `clean_loaded_value`
  tail calls `resolve_secret_value`.
- `crates/jcode-base/src/mcp/protocol.rs` — delegates to provider-env.
- `crates/jcode-base/src/provider_catalog.rs` — inline `api_key` resolution,
  warning gate.
- `crates/jcode-provider-openrouter-runtime` — `resolve_inline_api_key`
  wraps `profile.api_key` reads; regression test.
- `crates/jcode-base/src/config.rs` — `invalidate_config_cache` clears the
  substitution cache.
- `crates/jcode-app-core/` — dep + read sites in `channel.rs`,
  `notifications.rs`, `server/jade_relay.rs`, `tool/websearch.rs`.

## Upstream status

Strong upstream candidacy — generic feature, zero fork-specific behavior.
The `SubstitutionOutcome`-style stderr surfacing on the MCP path is a small
observability improvement over upstream's `Stdio::null`.
