# [29] Per-model context window + compaction threshold

Give every `[[providers.<name>.models]]` entry independent control over its
context window and its compaction trigger. Three example intents that share
this one schema:

```toml
# Claude via a relay — 1M real window, fold at ~272K
[[providers.sub2api-anthropic.models]]
id = "claude-opus-5-5"
context_window = 1_000_000
compaction_threshold_tokens = 272_000

# GPT via a Responses relay — OpenAI server-side compaction at 272K
[providers.sub2api-openai]
type = "open-ai-compatible"
api = "openai-responses"

[[providers.sub2api-openai.models]]
id = "gpt-6.1-sol"
context_window = 1_000_000
compaction_threshold_tokens = 272_000

# deepseek — no overrides, untouched by the new fields
[[providers.deepseek.models]]
id = "deepseek-v4.1-flash"

[provider]
openai_native_compaction_mode = "auto"
```

## Semantics

`context_window` (existing field, semantics strengthened):

- **Absolute override.** Whatever the user writes is the model's real
  context window — it beats the built-in Claude-generation table, the live
  provider catalog, and every other fallback. This field currently feeds the
  UI meter, the reactive-recovery ceiling, and `provider.context_window()`;
  after this patch all consumers honor the same precedence.
- Priority: `models[].context_window` > the channel's existing
  catalog/static defaults. Unconfigured models keep the existing resolution
  order.
- A model's value is authoritative for *that* model. If a provider lists
  `claude-opus-5-5` with `context_window = 340_000`, no code path may
  silently substitute the 1M default the static table claims.

`compaction_threshold_tokens` (new field):

- The soft compaction trigger in tokens. Local and native channels read
  the same field:
  - Local jcode compaction compares the larger of its token estimate and
    provider-reported input tokens against `T` in reactive mode. Proactive
    and semantic modes retain their existing prediction/anti-signal rules
    with a `T`-based soft threshold.
  - The safety budget remains the real `context_window`, subject to the
    existing `[compaction].max_context_tokens` cap. The 95% hard-protection
    threshold and emergency retention do not shrink to `T / 0.8`.
  - Native OpenAI auto compaction sends `T` as
    `context_management[].compact_threshold`.
  - A named profile with `api = "openai-responses"` opts into that native
    path only for GPT ids carrying this field, and only when
    `[provider].openai_native_compaction_mode = "auto"`. Other models,
    chat/completions, and unset fields retain their previous local behavior.
- Values have a 1,000-token minimum and are capped by the channel's context
  budget. An active native threshold wins over an `extra_body` value for
  `context_management`.
- Unset preserves the channel's existing default:
  `provider.openai_native_compaction_threshold_tokens` for the OpenAI
  channel, the `context_window`-derived budget for jcode folding.

## Scope, non-goals

- No new config surface beyond the two `[[providers.*.models]]` keys.
- No routing changes for unconfigured models. Configured GPT models on a
  Responses profile use server-side auto compaction instead of jcode folding.
- `prompt_caching` / `service_tier` / etc. stay at the provider level; this
  patch doesn't generalise unrelated fields.
- The global `provider.openai_native_compaction_*` knobs stay — they remain
  the documented fallback for models that don't opt in.

## Implementation notes

- `NamedProviderModelConfig` (in `jcode-config-types`) gains
  `compaction_threshold_tokens: Option<usize>`. `context_window` keeps its
  existing aliases.
- Explicit overrides are read from the current config, not copied into a
  second context cache. Provider hints and recognized `profile:model`
  prefixes select the correct profile; conflicting or ambiguous matches
  do not borrow another profile's values. Exact model ids precede basename
  fallback, and legitimate colons inside model ids are preserved.
- Local manager configuration refreshes the real window and separate soft
  threshold before requests, on model switches, and on session restoration.
  Saving/removing an override reaches existing sessions without restart.
- Anthropic, OpenAI and compatible runtime window lookups honor explicit
  model overrides before their own defaults/catalogs.
- Compatible Responses uses its instance's stable profile identity, not
  process-global `JCODE_RUNTIME_PROVIDER`, to resolve the native threshold.
  The shared Responses parser and session persistence replay encrypted
  compaction items on subsequent requests.

## Acceptance

1. `claude-opus-5-5` with `context_window=1_000_000` and
   `compaction_threshold_tokens=272_000` — UI shows 1M, folding starts at
   272K in reactive mode when message guards permit; 323K does not hard-drop
   history, and hard protection remains at 950K for the 1M window.
2. `gpt-6.1-sol` on a named Responses profile with the same numbers —
   outbound request carries
   `context_management[].compact_threshold = 272_000`, no jcode folding.
3. `deepseek-v4` with no `compaction_threshold_tokens` — token budget,
   UI meter, and recovery behavior identical to a build without the patch.
4. `context_window=340_000` on `claude-opus-5-5` (a model the static table
   would classify as 1M) — every consumer reports 340_000.
5. `cargo test -p jcode-config-types -p jcode-base -p jcode-app-core` plus
   the provider crate tests for the request payload stay green.
6. Same-named models under two profiles keep distinct windows/thresholds;
   qualified requests select the right profile without a hint. Removing an
   override restores the channel default instead of leaving a stale value.

## Upstream status

General per-model configuration and compaction-budget separation are upstream
candidates. The fixes are carried within this patch, not as unrelated upstream
bug-fix commits. No new config aliases or `/config` display entries are added.
