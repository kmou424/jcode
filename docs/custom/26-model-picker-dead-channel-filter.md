# [26] Model picker hides channels with no usable credentials

## What it does

The `/model` picker groups every route a model can be served through by
channel (`api_method` = one credential slot). Upstream lists dead channels
alongside live ones; a dead channel renders rows the user can never
successfully switch to (openai-api with no key, claude-oauth with no
login, a copilot token that expired on the daemon).

This patch adds `drop_unavailable_channel_routes` in
`crates/jcode-tui/src/tui/app/inline_interactive.rs`, applied to the route
list before the picker is built. A channel is dead when *every* route
carrying that `api_method` reports `available: false`; dead-channel
routes are then removed, which hides both the channel's extra option rows
and any model reachable only through it.

`ModelRoute.available` already encodes credential sufficiency
(`is_configured`-style checks on the owning side), and the remote daemon
pushes routes with the flag evaluated against the daemon's credentials,
so remote filtering uses remote truth, not local auth state.

## Exemptions

- **Placeholder methods** (`current`, `remote-catalog`) are picker
  scaffolding, not credentials — never dropped.
- **The current model keeps every route** so its row never collapses
  mid-load or while reconnecting.
- **Fully-dead catalogs stay visible**: if no channel is live at all the
  filter would empty the picker, which is strictly worse than showing
  dead rows, so it is skipped wholesale.
- **Synthesized fallback routes are skipped**: names-only remote catalogs
  (`remote_model_routes_fallback` / the lightweight variant) guess
  `available` from *local* auth state, which is meaningless under a remote
  daemon. Any list where every route carries the
  `"fallback: static provider model list"` detail is left alone. Mixed
  lists still filter — synthesized extras keep whatever availability their
  builder guessed, which is self-consistent.

Config-declared models (`[providers.<name>.models]`, static openai-compatible
lists) emit `available: true` by construction, so they always show.

## Call sites

The filter runs at every point an *authoritative* route list enters the
picker inside `open_model_picker_inner`, and in the
`debug_model_picker_live_json` mirror of the same pipeline:

- local `simplified_model_routes_for_picker` / `provider.model_routes()`
  outputs,
- remote `remote_model_options` (daemon-pushed routes) after
  `extend_remote_routes_for_uncovered_models`.

It intentionally does **not** run inside `open_model_picker_with_routes`
itself — that function also receives synthesized fallback lists where
filtering is wrong (see above).

## Tests

- `test_model_picker_hides_models_reachable_only_through_dead_channels`
- `test_model_picker_strips_dead_channel_options_but_keeps_shared_model`
- `test_model_picker_keeps_current_model_when_its_channel_died`
- `test_model_picker_fully_unavailable_catalog_stays_visible`

Existing remote-picker tests that leaked ambient state were wrapped in
`with_temp_jcode_home` (and `JCODE_NAMED_PROVIDER_PROFILE` cleared where
the fallback consults it): a persisted catalog cache or a leaked named
profile env makes the picker take the authoritative path with
environment-provided routes and hides the rows the test asserts on.

## Affected files

- `crates/jcode-tui/src/tui/app/inline_interactive.rs`
- `crates/jcode-tui/src/tui/app/tests/remote_startup_input_01/part_02.rs`
- `crates/jcode-tui/src/tui/app/tests/remote_startup_input_02/part_01.rs`
- `crates/jcode-tui/src/tui/app/tests/state_model_poke_02/part_02.rs`

## Upstream candidacy

Yes — channel-level dead-route pruning is a generic UX improvement, not
fork-specific config. The exemption rules (placeholder/current/all-dead/
synthesized) encode real edge cases upstream would need too; the test
fixture isolation fixes are also upstreamable. PR candidate.
