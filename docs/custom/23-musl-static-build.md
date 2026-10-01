# [23] musl static builds + fork self-update

## What it does

Two pieces that together make `jcode update` work end-to-end on the fork:

1. **musl static assets.** `x86_64-unknown-linux-musl` /
   `aarch64-unknown-linux-musl` binaries built by `scripts/build_musl.sh`
   (native musl-gcc when a musl toolchain is installed; `rust:alpine`
   docker fallback otherwise). A musl binary is fully static — it
   runs on Alpine, NixOS (no `/lib64/ld-linux*`), and every glibc distro, so it
   is the universal Linux asset.
2. **Fork self-update.** `update.rs`/`install.sh`/`install.ps1`/selfdev clone
   all point at `kmou424/jcode`; the updater picks the newest *strict-semver*
   tag from `/releases` (non-draft, non-prerelease, max wins).

## Release model (per-branch fork convention)

- Each `kmou424/v<tag>` patch branch owns release tag `v<tag>` (strict
  semver matching the packaged upstream version). `local-release.yml`
  repackages that tag on every branch push; the tag ref is moved to the
  new commit and the release recreated, so `target_commitish` always
  marks the line's latest packaged state. Its build matrix covers the
  updater's full asset naming (`jcode-linux-x86_64`,
  `jcode-linux-aarch64`, `jcode-macos-aarch64`,
  `jcode-windows-x86_64.exe`, plus both musl targets) and publishes
  `SHA256SUMS` so `install.sh`'s checksum gate has material.
- `jcode update` picks the max strict-semver tag from `/releases` — a
  newer upstream line wins automatically. Same-tag repackages are
  detected by comparing the release's `target_commitish` against the
  binary's embedded `GIT_HASH` (equal semver only — an older release
  never pulls the install backwards).
- Development (`*-dev`) builds use the release's `release-info.json`
  `commit` field — the exact commit the binaries were packaged from —
  with `target_commitish` as fallback. They update when the packaged
  commit differs and is not an ancestor of the embedded `GIT_HASH`,
  covering both a newer version line and a same-tag repackage.
  Upstream's ancestry guard assumed linear history; on the fork a
  rebased or cherry-picked line always looks "diverged", so divergence
  permits installing as long as the compiled commit is reachable from a
  remote ref (locally, or on GitHub). A diverged commit no remote ref
  contains — unpushed local work — refuses with an explicit error
  instead of silently discarding it. A `404` from the compare endpoint
  (compiled sha unknown to GitHub) refuses the same way.
- Rollback is manual: `scripts/install.sh v<older-tag>` (each version
  line keeps its own tag).
- `scripts/package_fork_release.sh v<tag>` does the whole loop locally
  from `--ref` (default `HEAD`): builds host gnu arch + both musl
  arches via `build_musl.sh` in a detached worktree, emits `SHA256SUMS`,
  moves the tag ref, recreates the release. `--dry-run`, `--repo`,
  `--targets` for control.
- Version is baked at compile time via `JCODE_BUILD_SEMVER` (same
  mechanism as CI), so an installed binary reports its tag.

## Code surface

- `get_asset_name()` (`jcode-update-core`): `target_env="musl"` branches return
  `jcode-linux-musl-<arch>` — a musl binary can never overwrite itself with a
  glibc asset it cannot run.
- `fetch_latest_release_blocking` (`update.rs`): `/releases?per_page=100` +
  `select_latest_release` (non-draft, non-prerelease, strict `v?X.Y.Z`,
  max wins). `GitHubRelease` gained `draft`/`prerelease` fields.
- `install.sh`: `linux_needs_musl()` — `/etc/NIXOS` present or the arch's
  ld-linux interp missing → musl asset.
- Same-tag repackage detection: a release build compares its embedded
  `GIT_HASH` against the release's `target_commitish`; a differing hex sha
  means the tag was recreated on a newer patch state, so `jcode update`
  reinstalls even though the semver is unchanged. Branch-name commitishes
  and unavailable embedded hashes cannot prove the tag moved and fall back
  to plain semver comparison.
- `fetch_release_pkg_commit` + `fetch_release_info_blocking`
  (`update.rs`): download `release-info.json` from the release assets
  for dev-build decisions; `update_dev_guard::should_install_release`
  takes `(tag, release_commit)` and decides locally when both commits
  resolve in `get_repo_dir()` (release ahead → install, dev ahead →
  keep, diverged → install iff the compiled commit is on a remote ref),
  else via GitHub `compare/{packaged}...{embedded}`
  (`behind`/`diverged` → install, `ahead`/`identical` → keep, 404 →
  error).
- `GITHUB_REPO`/`REPO`/`$Repo`/`JCODE_REPO_URL` → `kmou424/jcode`
  (`update.rs`, `install.sh`, `install.ps1`, `src/cli/selfdev.rs`,
  `tool/selfdev/mod.rs`).
- `release.yml`: two `musl_container` matrix entries (native-arch runners),
  `musl-tools` installed so `build_musl.sh` takes its native path and the
  job's rust-cache applies, non-container
  steps gated off for musl, `ssh-agent` guarded on `DEPLOY_KEY` for forks.
  musl assets join SHA256SUMS + attach via the existing artifact glob; they are
  intentionally NOT in the required-assets list so a musl failure never blocks
  a release.

## Caveats

- musl caveat (documented upstream-style): NSS/DNS uses musl semantics —
  `getaddrinfo` reads only `/etc/hosts`+resolv.conf; nscd/NSS plugins are
  ignored. Fine for jcode's use (HTTP APIs).
- x86_64-musl on an arm64 host runs through qemu/binfmt — works, slow; CI is
  the fast path.
- `jcode update` on the fork never sees upstream releases anymore — that is
  the point (fork updates carry the patch series). Upstream news remains a
  manual `git fetch upstream --tags` concern.
- The `main` update channel (`update_channel = "main"`) only compares
  against the tip of the build's own `kmou424/v<PKG_VERSION>` branch, so
  a dev build on an older line still sees "up to date" there — use the
  `stable` channel to get cross-line upgrades.

## Files

- `scripts/build_musl.sh`, `scripts/package_fork_release.sh` (new)
- `crates/jcode-update-core/src/lib.rs` — musl asset names, strict semver
  selector + tests.
- `crates/jcode-app-core/src/update.rs` — repo repoint + list-based fetch
  + `release-info.json` packaged-commit resolution for dev builds.
- `crates/jcode-app-core/src/update_dev_guard.rs` — packaged-commit
  comparison (was: tag-ref ancestry).
- `scripts/install.sh`, `scripts/install.ps1` — repo + musl detection.
- `src/cli/selfdev.rs`, `crates/jcode-app-core/src/tool/selfdev/mod.rs` —
  clone repoint.
- `.github/workflows/release.yml` — musl matrix + guards.

## Upstream status

Musl pieces (asset naming, `build_musl.sh`, matrix, install detection) are
upstream-candidacy material. Repo repointing is fork-only by definition.
