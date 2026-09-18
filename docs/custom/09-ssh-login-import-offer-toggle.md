# [09] Feature flag for the SSH login-import startup offer

## Requirement

On SSH remote attach, the TUI probes the remote host's login status once
(`Operation::Status`) and, when the remote has zero configured logins,
opens a Yes/No offer to import a local OpenAI/Claude login. On hosts that
never want remote logins this popup is pure noise on every attach, and the
status probe is wasted work.

## Behavior

- New `[features] ssh_login_import_offer` (bool, default `true`).
- `false` makes `App::poll_ssh_login_onboarding` dismiss the onboarding
  state and return immediately: no remote status probe is spawned, so no
  offer picker can ever appear.
- The explicit import path is unaffected — `/login --import-local
  openai|claude` still runs the consented one-time copy.
- Default `true` preserves upstream behavior.

## Config surface

```toml
[features]
ssh_login_import_offer = false   # default true
```

## Affected files

- `crates/jcode-config-types/src/lib.rs` — `FeatureConfig` field +
  `Default`.
- `crates/jcode-tui/src/tui/app/auth_remote/onboarding.rs` — flag gate at
  the top of `poll_ssh_login_onboarding`; regression test
  `ssh_onboarding_flag_disabled_suppresses_probe_and_offer` (writes
  `[features] ssh_login_import_offer = false` under the test `JCODE_HOME`,
  asserts no probe task and no picker).

## Upstream status

Fork-local UX knob; not proposed upstream (upstream may prefer a config
knob too, but the request here is a local hard-off preference).
