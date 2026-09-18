# Local Patch Maintenance Rules (kmou424 fork)

This working copy is the kmou424 fork of jcode. When doing self-dev work here,
follow the fork's patch management rules:

## Repository layout

- Fork remote (read-write): `git@github.com:kmou424/jcode.git` (`origin`).
- Upstream (read-only): `https://github.com/1jehuang/jcode.git` (`upstream`).
  Fetch tags from it.
- Patch branch naming: `kmou424/<upstream-tag>` (e.g. `kmou424/v0.84.0`).
  Each branch is based on a specific upstream tag — never on a moving
  `master`.

## Commit conventions

- **Docs commit first.** The first commit on every `kmou424/<tag>` branch is
  the fork-documentation commit with the fixed subject
  `docs(fork): kmou424 patch series`. It carries `.jcode/prompt-overlay.md`
  (this file), everything under `docs/custom/`, and all files under
  `.github/workflows/`. All later changes to docs, overlay, or workflow
  files are amended into that same first commit; the rest of the branch is
  then rebased on top and the branch force-pushed. Never scatter these
  changes across multiple commits. The fixed subject is how CI and agents
  locate the commit (`git log --fixed-strings --grep='docs(fork): kmou424 patch series'`).
- **Feature patches carry an ID prefix.** Every patch commit message starts
  with `[<ID>]` where `<ID>` is a two-digit number matching the feature's
  doc file (`docs/custom/<ID>-<slug>.md`). This is the only intentional
  deviation from conventional-commit style. Example:
  `[01] feat(providers): ...` pairs with `docs/custom/01-*.md`.
- Each patch commit is a self-contained feature/fix; they get cherry-picked
  and reordered, so avoid mega-commits mixing unrelated changes.
- When a change is a candidate for upstream (general feature/fix, not local
  config), note it in the doc and in the commit body so it can be PR'd and
  eventually dropped from the local series.

## Upgrade workflow

1. `git fetch upstream --tags`
2. Create the new patch branch: `git checkout -b kmou424/<new-tag> <new-tag>`
3. Cherry-pick the previous branch's commits in order — docs commit first,
   then numbered patches.
4. `git rebase -i` to group related patches: commits serving the same
   feature/requirement should be adjacent (and squashed when they are one
   logical change).
5. For every patch, check whether upstream now covers the feature or
   conflicts with its design. If upstream already implemented it, drop the
   patch and its doc. If the design diverged, flag it to the user and
   negotiate before rewriting.
6. Amend the docs commit if any doc or these rules changed, rebase, then
   force-push `kmou424/<new-tag>` to `origin`.

## Documentation layout

- `docs/custom/` holds one numbered doc per patch/feature:
  `<ID>-<slug>.md`, e.g. `01-named-provider-display-name.md`. The doc is the
  requirement-side reference for the patch: what it does, why, config
  surface, affected files, upstream status. Keep the set small — one doc
  per logical feature, not per commit or per file.
