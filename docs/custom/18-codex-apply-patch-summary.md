# [18] Codex-equivalent `apply_patch` result text for GPT models

## What it does

The freeform `apply_patch` variant ([13]) switches the tool result text
returned to the model to codex's exact `print_summary` format when the
session's model id contains `gpt` (case-insensitive):

```
Success. Updated the following files:
A added.txt
M modified.txt
D deleted.txt
```

one `A`/`M`/`D` line per affected path, grouped adds → modified →
deletes. A `*** Move to:` hunk counts as `M` on the **source** path —
identical to codex (`print_summary` iterates `affected.modified`, which
receives `hunk.path()`, the patch's source path). The per-hunk `✓`/`✗`
lines, hunk counts and the unified `File diff:` code block are dropped.

Hunks that fail still apply jcode's continue-past-error semantics (codex
would abort the whole patch); they surface as `error: <path>: <err>`
lines after the summary instead of `✗` lines. A patch that produces no
changes keeps the shared `No changes applied` output.

All other cases are byte-identical to before: the compat variant never
runs the model check, and freeform sessions on non-GPT models keep the
`✓`-lines + `File diff:` output.

## Why

GPT models are trained on codex's apply_patch and expect its result
text. Matching it avoids confusing the model with jcode's richer summary
format while keeping jcode semantics everywhere else. Detection uses the
[16] session-model side table (`session_model::session_model`), which is
populated for every session regardless of provider.

## Design notes

- The check happens inside `ApplyPatchFreeformTool::execute` — execution
  time, not registration time — so a mid-session model switch flips the
  format for subsequent calls automatically.
- `apply_patch_text` takes a `codex_style` flag; the compat `execute`
  passes `false` unconditionally. A/M/D groups and failure lines are
  collected alongside the legacy `results` vec; only the selected one is
  emitted.
- In codex mode the unified-diff assembly is skipped entirely, so the
  `File diff:` block cannot leak into the result text.

## Config surface

None — detection is automatic on the session model id.

## Affected files

- `crates/jcode-app-core/src/tool/apply_patch.rs` (flag threading,
  codex-style result assembly)
- `crates/jcode-app-core/src/tool/apply_patch_tests.rs` (codex-format
  and byte-identical-legacy tests)

## Upstream status

Upstream-candidate: the behavior is a strict subset keyed on model id
and defaults to the existing output; a PR could carry it as an opt-in
or always-on heuristic. Not submitted upstream.
