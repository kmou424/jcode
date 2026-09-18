# [16] `git_commit` / `git_checkpoint` structured commit tools

## What it does

Ports the dsh-plugins git tools: the agent creates conventional-commit
messages through a typed tool call instead of free-form `git commit` shell
commands, so message rules (type enum, scope charset, description style,
trailers, Refs/Closes) are enforced by the tool rather than by prompt
discipline. Registered unconditionally on the full tool profile — no
experimentals flag.

- `git_commit(paths? | checkpoint?, type, scope?, description, body[]?,
  closes[]?, refs[]?, repoPath?, allowDefaultBranch?, includePreStaged?)`
  — assembles `<type>(<scope>): <desc>` + `- bullet` body + `Closes #n` /
  `Refs: #n` lines + `Co-Authored-By` trailer, then stages the given paths
  (or promotes a checkpoint ref) and commits. Issue references (`Closes`,
  `Refs`) form one footer block and the `Co-Authored-By` trailer sits in
  its own block after a blank line, so the sign-off stays visually
  separate from issue metadata.
- `git_checkpoint(message, includeUntracked?, repoPath?)` — `git stash
  create` + `git stash store` snapshot that does not disturb the worktree;
  pass its ref to `git_commit.checkpoint` to promote it into a real
  commit.

## Gates (in order)

mutual exclusion of `paths`/`checkpoint` → type enum (11 types) → scope
`^[a-z0-9._/-]+$` → description (non-empty, trimmed, no `.!?。！？`
ending, lowercase first, ≤72 chars) → repo root resolution →
default-branch refusal (remote origin/HEAD → init.defaultBranch → "main";
`allowDefaultBranch` bypass) → checkpoint apply or path normalization →
foreign-staged refusal (`includePreStaged` bypass) → stage →
nothing-staged refusal → trailer → commit.

## Input tolerance

Provider-side strict schema normalization marks every property required and
expresses optionality as nullable, so models legitimately emit `null`, `""`,
or `[]` for fields they are not using (observed: `checkpoint:""` tripping the
mutual-exclusion gate, then explicit `null`s failing serde outright, then a
permanent bash-git fallback). Optional fields therefore deserialize leniently
via `serde_coerce` helpers: `null`/`""`/`[]` all collapse to absent —
`opt_vec_nonempty` (`paths`), `opt_string_blank_as_none` (`checkpoint`,
`scope`, `repoPath`), `null_default` (`body`, `closes`, `refs`, the two
booleans). Blank `paths` elements are dropped rather than normalized to `.`
(which would silently stage the whole repo); blank `body` lines and `0` issue
numbers are skipped during message assembly. `current_branch` resolves via
`symbolic-ref` first so an unborn HEAD (orphan root commit) works; detached
HEAD still falls back to `rev-parse --abbrev-ref`.

## Sign-off trailer

`[tools.git]` config (`GitToolsConfig` on `ToolConfig`):

```toml
[tools.git]
signoff_name = "${model}"
signoff_email = "${model}@local"
```

`${model}` resolves through a new per-session model-identity side-table
(`crates/jcode-app-core/src/session_model.rs`, same deadlock-free pattern
as `session_effort`) keyed by `ctx.session_id`, recorded at every
model/route mutation site; `${user}` resolves from `git config
user.name`/`user.email`. An unresolvable placeholder is a hard error;
unset fields mean no trailer. Implementation note: the spec asked for the
model on `ToolContext`, but `ToolContext` has ~85 literal construction
sites — the side-table reaches the same identity without touching them.

## Affected files

- `crates/jcode-app-core/src/tool/git.rs` + `git_tests.rs` (new, ~720 LoC
  impl + 26 tests)
- `crates/jcode-app-core/src/tool/serde_coerce.rs` (`null_default`,
  `opt_string_blank_as_none`, `opt_vec_nonempty` lenient deserializers)
- `crates/jcode-app-core/src/session_model.rs` (new)
- `tool/mod.rs`, `agent.rs`, `agent/provider.rs`, `agent/status.rs`,
  `agent/turn_execution.rs`, `agent/turn_streaming_mpsc.rs`,
  `server/client_disconnect_cleanup.rs` (record/forget sites)
- `crates/jcode-base/src/config.rs` (`GitToolsConfig`)

## Upstream status

dsh-plugins port; the structured-commit concept could be PR'd upstream.
Folds the same upstream `duration_secs` test-literal fix as [12]/[17]
(upstream-fix candidacy). Remaining same-class drift in other test
targets is tracked for a separate `[upstream-fix]` patch.
