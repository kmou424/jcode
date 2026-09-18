# [13] Freeform `apply_patch` + compat variant + edit-family exclusion

## What it does

Splits `apply_patch` into two experimentals-gated variants (see [12]) and
makes the patch surface mutually exclusive with the built-in editing
family:

- `tool_apply_patch` — freeform variant. On the Responses wire it is
  declared as a `custom` tool carrying a Lark grammar
  (`{type:"custom", name, description, format:{type:"grammar",
  syntax:"lark", definition}}`), matching codex-rs's apply_patch tool;
  capable models emit raw `*** Begin Patch` text. On
  chat-completions/anthropic/gemini wires the marker rides inside the
  input schema (`x-jcode-freeform`) and the tool degrades to an ordinary
  function tool with a single `input` string parameter.
- `tool_apply_patch_compat` — JSON variant for distilled models that
  cannot drive a freeform `custom` tool; an ordinary function tool with a
  `patch_text` string parameter, same parse/exec engine.
- Neither tag → `apply_patch` is not registered. Either tag →
  `edit`/`multiedit`/`write`/`patch` are removed from the session tool
  map (and restored when the tags go away, e.g. on model switch). Both
  tags → configuration error surfaced to the user; neither variant is
  enabled and the editing family stays.

## Design notes

- Variant distinction rides on the advertised definition:
  `ToolDefinition::freeform_format()` reads the `x-jcode-freeform`
  schema annotation, so the dispatcher's idempotent apply can tell which
  variant is installed under the shared `apply_patch` registry name.
- Replay on the Responses wire: stored tool calls shaped
  `{"input": "<patch>"}` (the stream layer wraps custom_tool_call text
  into that shape) are emitted as `custom_tool_call` items; compat calls
  store `{"patch_text": ...}` and replay as ordinary `function_call`s.
  The check `name == "apply_patch" && input.get("input").is_string()`
  cannot confuse the two (a compat call without `patch_text` would have
  failed validation anyway). Known edge: switching tags mid-session
  leaves old freeform calls replaying as `custom_tool_call` under a
  function declaration — worst case a provider 400, never silent
  misapplication.
- Parse/exec engine is shared with the pre-existing JSON `ApplyPatchTool`
  (`tool/apply_patch.rs`, heavily reworked): `*** Begin Patch` /
  `Add|Update|Delete File` / `*** Move to:` ops, fuzzy context matching,
  and the same diff-preview path the built-in editing tools render
  through (`tool/file_diff.rs`).

## TUI diff display

`apply_patch` tool messages render the *effective edit result* in the
same numbered `N±` shape the built-in editing tools use. Parsing lives
in `ui_diff.rs` behind `diff_lines_for_tool_message`:

- Preferred source: the post-edit result text
  (`collect_apply_patch_result_diff_lines`). Its `File diff:` fenced
  unified diff is parsed per file — `+++`/`---` headers supply the
  filename, `@@ -a,b +c,d @@` headers supply real line numbers and hunk
  boundaries. Each change is emitted exactly once (the result text
  duplicates it as a `N±` numbered echo plus the fenced diff; only the
  fenced copy is used). Second and later hunks of one file are
  separated by a dim `│ @@ -a,b +c,d @@` row (`DiffLineKind::Sep`);
  `✗ path:` gates emit a red `│ ✗ Error: <msg>` row
  (`DiffLineKind::Error`) in their own file group so a failed hunk
  keeps its position and its diff window.
- Per-file fallback: `✓ path:` gates collect the `N±` numbered echo for
  files the fenced diff omits; legacy bare-`@@` sections under a gate
  are numbered from their hunk headers.
- Content fallback: generic `collect_diff_lines` for foreign result
  shapes, then `collect_apply_patch_diff_lines` on the patch input —
  a dedicated codex-grammar scanner (markers only count at column 0,
  `*** Add/Update/Delete File:` set file boundaries) so a hunk content
  line starting `--- `/`+++ ` can no longer be misread as a file
  boundary and silently dropped from both the rows and the `+N -M`
  badge.
- `diff_change_counts_for_tool` counts the same rendered rows for
  `apply_patch`, so the badge can never disagree with the box. Both
  `input` (freeform) and `patch_text` (compat) keys are accepted for
  the input fallback and the `file_path_for_ext` filename probe.
- `DiffLineKind` gained `Sep`/`Error` variants; the side-by-side file
  diff overlay flushes hunks on `Sep`.
- Stored tool messages wrap the result in `[apply_patch] `, so the
  `✓`/`✗` gate parse tolerates one leading `[name] ` prefix — otherwise
  a failed hunk produced no section and its `✗ Error:` row never
  rendered.
- `tint_span_with_diff_color` blends diff-dominant (70% row color +
  30% syntax). Syntax-dominant let a markdown `- ` list bullet
  (`markup.list` red in base16-ocean.dark) turn an added `.md` line
  visibly red — a deletion look-alike.

## Config surface

```toml
[[providers.<profile>.models]]
id = "gpt-6-astra"
experimentals = ["tool_apply_patch"]          # freeform
# experimentals = ["tool_apply_patch_compat"] # JSON variant (exclusive)
```

## Affected files

- `crates/jcode-app-core/src/experimentals.rs` (EDIT_FAMILY_TOOLS,
  `patch_tag_conflict`, two-way family swap) +
  `experimentals/tool_apply_patch{,_compat}.rs`
- `crates/jcode-app-core/src/tool/apply_patch.rs` (+freeform tool) &
  `apply_patch_tests.rs`
- `crates/jcode-message-types/src/lib.rs`
  (`TOOL_FREEFORM_FORMAT_KEY`, `freeform_format()`)
- `crates/jcode-provider-openai/src/request.rs` (`custom` tool emission +
  `custom_tool_call` replay), `stream.rs`, `stream_tool_tests.rs`
- `crates/jcode-app-core/src/agent.rs`, `agent/turn_execution.rs`
  (conflict surfacing)
- `crates/jcode-tui/src/tui/ui_diff.rs` (`DiffLineKind` Sep/Error,
  `collect_apply_patch_result_diff_lines`, `collect_apply_patch_diff_lines`,
  `diff_lines_for_tool_message`, `diff_change_counts_for_tool`, tests)
- `crates/jcode-tui/src/tui/ui_messages.rs` (render colors, `input` key
  fallback for the filename probe)
- `crates/jcode-tui/src/tui/ui_file_diff.rs` (hunk split on `Sep`)
- `crates/jcode-tui/src/tui/ui_prepare.rs` (`input` key fallback)
- `crates/jcode-tui/src/tui/ui.rs` (re-exports)

## Upstream status

Upstream candidate for the framework half; the freeform/custom-tool wire
plumbing mirrors codex-rs behavior. The diff-display half is upstream-fix
candidacy: upstream's generic `collect_diff_lines` has no hunk state
machine, so `--- `/`+++ ` hunk-content lines are misread as file
boundaries, apply_patch results render each change twice (numbered echo
plus fenced diff), the `+N -M` badge can disagree with the box, and
freeform calls under `input` lose their filename label. On each version
bump check whether upstream made the diff parser hunk-aware or added an
apply_patch-specific path; drop this part when it has.
