# [19] Escaped brackets inside reasoning markup are not LaTeX math

## What it does

Thinking text containing bracketed tokens such as `[03]` or `[NN]` no
longer renders as a display-math box. `reasoning_line_markup` escapes
`[`/`]` to `\[`/`\]` inside the sentinel-wrapped emphasis run
(`*<U+2063>…<U+2063>*`) so bracketed text survives link parsing;
upstream `normalize_latex_math` cannot tell those escapes from real
LaTeX `\[` delimiters and promoted them to `$$…$$`, which the renderer
then drew as a `┌─ math` block.

The fix tracks the `\u{2063}` reasoning sentinel inside
`normalize_latex_delimiters_and_environments` and skips the `\(`/`\[`
delimiter conversion while inside a sentinel-wrapped run. A second
same-class hole is closed at the same time: the `\begin{env}`
conversion branch lacked the `is_escaped_at` check every other
delimiter branch has, so a literal `\\begin{align}` was promoted to
math as well.

## Why

Patch-id tokens like `[03]` are ubiquitous in this fork's thinking
traces (commit subjects, doc names); every one of them exploded into a
math box mid-thinking.

## Design notes

- The guard lives in `preprocess.rs`, so all three consumers
  (`markdown_render_full`, `markdown_render_lazy`, and render-core's
  own markdown path) inherit it.
- The sentinel toggle is a single `\u{2063}` count. Reasoning markup
  only ever emits the char in balanced pairs and it has no other
  source, so an odd count means inside a reasoning run.
- `\)` and `\]` closers need no guard: they only fire while a
  conversion is already open, which the opener guard now prevents in
  reasoning runs. `$`-delimiters were never affected because `$` is
  escaped too (`\$` → `is_escaped_at` blocks it).

## Config surface

None.

## Affected files

- `crates/jcode-render-core/src/preprocess.rs` (sentinel tracking,
  missing escape guard on the environment branch, regression tests)
- `crates/jcode-tui-markdown/src/markdown_tests/cases/rendering.rs`
  (end-to-end reasoning-line case)

## Upstream status

Upstream bug — both `REASONING_ESCAPES`/`reasoning_line_markup` and
`normalize_latex_math` exist verbatim in upstream v0.86.0. This is an
`[upstream-fix]` patch: on each version bump check whether upstream
fixed the `\[`→`$$` promotion inside reasoning markup and drop the
patch when it has.
