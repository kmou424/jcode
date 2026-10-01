# [28] Generic prompt-overlay hook + delegated slash commands

Two extension points that let external tools plug into jcode without touching
its source. The immediate motivation is the **ponytail** prompt-engineering
plugin (`~/.jcode/plugins/ponytail` submodule), but both surfaces are generic —
nothing in jcode mentions ponytail by name.

## `prompt_overlay` hook

A new `[hooks]` event. Commands registered under it run once per turn, before
the system prompt is built; each command's stdout is concatenated (16 KiB cap)
and appended to the **dynamic** part of the system prompt under a
`# Prompt Overlay` heading.

- Env: `JCODE_HOOK_EVENT=prompt_overlay`, `JCODE_HOOK_SESSION_ID`,
  `JCODE_HOOK_CWD`, `JCODE_HOOK_TURN`, `JCODE_HOOK_PAYLOAD`.
- `prompt_overlay_timeout_ms` (default 500) bounds each hook — mode control is
  interactive, so a slow script just means "no overlay this turn" (fail-open).
- Because the system prompt is rebuilt per turn, mode changes via the flag
  file take effect on the very next turn — no mid-session restart needed.
- Only the turn-loop call sites use it (`build_system_prompt_split_with_overlay`).
  `prewarm_provider_idle` and debug paths keep the sync `build_system_prompt_split`.

`Config::slash_commands` is `BTreeMap<String, SlashCommandEntry>` on the base
config; `HooksConfig` gains `prompt_overlay` + `prompt_overlay_timeout_ms`.

## `[slash_commands]` delegation

```toml
[slash_commands.ponytail-ctl]
command = "bash ~/.jcode/scripts/ponytail-ctl.sh"
description = "Control the ponytail prompt-overlay mode"
```

`/name args…` dispatches to `command args…` (5 s timeout). Stdout is parsed as
JSON `SlashCommandAction`:

| `type` | Effect |
|---|---|
| `silent` | nothing |
| `display` | `DisplayMessage::system(text)` |
| `error` | `DisplayMessage::error(text)` |
| `activate_skill` | sets `app.active_skill`, optional `prompt` lands in input |
| `display_and_activate` | display + activate |

Non-JSON output falls back to `display`; empty output is `silent`.

### Autocomplete

`command --describe` is invoked once per `(name, command)` pair and cached in a
`OnceLock` — the schema's `subcommands[]` (with nested `args[].values[]`) feeds
`complete_from_spec`. If `--describe` is missing/fails, jcode falls back to
running `command __completions <completed-args…> <prefix>` with a 500 ms
timeout. The result rows rebuild the full `/name …` candidate line so Tab
replaces the in-flight token.

The env for both code paths is the standard `JCODE_HOOK_*` set plus
`JCODE_SESSION_ID` so scripts invoked via the bash tool can address the same
session flag (`tool/bash.rs` exports `JCODE_SESSION_ID` to every subprocess).

## Ponytail wiring (in `~/.jcode`, not the repo)

- `plugins/ponytail` is a git submodule pinned at upstream `v4.2.0-195`.
- `skills/ponytail*` are symlinks into the plugin so upstream `/ponytail`,
  `/ponytail-review`, … keep their skill-activation semantics.
- `scripts/ponytail-overlay.sh` resolves the mode chain (session flag → global
  flag → `$PONYTAIL_DEFAULT_MODE` → `full`) and prints the mode's ruleset —
  filtered from `skills/ponytail/SKILL.md` by awk when node isn't available.
- `scripts/ponytail-ctl.sh` implements the `--describe` / `__completions` /
  `lite|full|ultra|off|status|default` actions. `/ponytail <level>` stayed
  upstream's, so mode control lives under `/ponytail-ctl` to avoid colliding
  with the skill name.
- `~/.jcode/.ponytail-mode` is the global flag (gitignored). Per-session flags
  live at `$CWD/.jcode/sessions/<session-id>/.ponytail-mode`.

## Upstream-fix candidacy

None — the whole patch is additive extension surface; no upstream bug fixes
were folded in.
