#!/usr/bin/env bash
set -euo pipefail

# Package a fork release for self-update: builds release binaries for the
# default target set (host glibc arch + both musl arches), then publishes a
# versioned GitHub release AND refreshes the fixed `latest` alias release on
# the fork repo. Updater picks the newest strict-semver tag, so versioned tags
# are the update channel; `latest` is a convenience alias only.
#
# Packaging always builds the given --ref (default: HEAD) in an isolated
# git worktree, so a dirty working tree or a checked-out side branch never
# leaks into a release binary.
#
# Usage:
#   scripts/package_fork_release.sh [--dry-run] [--ref HEAD] [--repo owner/name]
#       [--targets "x86_64-unknown-linux-musl ..."] v0.88.0
#
# Requires: cargo, docker (for musl targets), gh (authenticated to the repo).

REPO="kmou424/jcode"
REF="HEAD"
DRY_RUN=false
VERSION=""
TARGETS=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)   DRY_RUN=true; shift ;;
    --ref)       REF="${2:?--ref needs a value}"; shift 2 ;;
    --repo)      REPO="${2:?--repo needs a value}"; shift 2 ;;
    --targets)   TARGETS="${2:?--targets needs a value}"; shift 2 ;;
    -h|--help)
      sed -n '2,17p' "$0"; exit 0 ;;
    -*)
      echo "Error: unknown option $1" >&2; exit 1 ;;
    *)
      [[ -z "$VERSION" ]] || { echo "Error: one version argument only" >&2; exit 1; }
      VERSION="$1"; shift ;;
  esac
done

[[ -n "$VERSION" ]] || { sed -n '2,17p' "$0" >&2; exit 1; }

VERSION_NUM="${VERSION#v}"
if ! [[ "$VERSION_NUM" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Error: version must be strict semver vX.Y.Z (got '$VERSION')" >&2
  echo "       (fork release tags are pure semver so the updater can order them)" >&2
  exit 1
fi
TAG="v$VERSION_NUM"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
dist_dir="$repo_root/dist/fork-release-$TAG"
mkdir -p "$dist_dir"

# Resolve the ref to a commit so a moving branch name can never surprise us
# mid-run, and create the worktree at that exact commit.
ref_sha="$(git -C "$repo_root" rev-parse "$REF^{commit}" 2>/dev/null)" \
  || { echo "Error: cannot resolve ref '$REF'" >&2; exit 1; }
echo "Packaging $TAG from $REF ($ref_sha)"

host_arch="$(uname -m)"
case "$host_arch" in
  x86_64)          host_gnu="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64)   host_gnu="aarch64-unknown-linux-gnu" ;;
  *)               host_gnu="" ;;
esac

# Default target set: this machine's glibc build (native cargo) plus both musl
# arches (build_musl.sh; native musl-gcc when installed, docker otherwise).
# A foreign-arch musl build works through qemu/binfmt but is
# slow; drop it from --targets if the host lacks emulation.
if [[ -z "$TARGETS" ]]; then
  TARGETS="${host_gnu:+$host_gnu }x86_64-unknown-linux-musl aarch64-unknown-linux-musl"
fi
echo "Targets: $TARGETS"

artifact_name() {
  case "$1" in
    x86_64-unknown-linux-gnu)   echo "jcode-linux-x86_64" ;;
    aarch64-unknown-linux-gnu)  echo "jcode-linux-aarch64" ;;
    x86_64-unknown-linux-musl)  echo "jcode-linux-musl-x86_64" ;;
    aarch64-unknown-linux-musl) echo "jcode-linux-musl-aarch64" ;;
    *) echo "" ;;
  esac
}

run() {
  if $DRY_RUN; then echo "+ $*"; else "$@"; fi
}

worktree="$repo_root/.scratch/release-build-$TAG"
cleanup() {
  git -C "$repo_root" worktree remove --force "$worktree" 2>/dev/null || true
}
trap cleanup EXIT

if ! $DRY_RUN; then
  git -C "$repo_root" worktree add --detach "$worktree" "$ref_sha"
fi
build_root="$worktree"
$DRY_RUN && build_root="$repo_root"

# ---------------------------------------------------------------------------
# Build every requested target
# ---------------------------------------------------------------------------
for target in $TARGETS; do
  artifact="$(artifact_name "$target")"
  [[ -n "$artifact" ]] || { echo "Error: unknown target '$target'" >&2; exit 1; }
  echo "▸ $target → $artifact"

  case "$target" in
    *-linux-musl)
      run env JCODE_RELEASE_BUILD=1 JCODE_BUILD_SEMVER="$VERSION_NUM" \
        "$repo_root/scripts/build_musl.sh" "$target" "$dist_dir"
      ;;
    *-linux-gnu)
      [[ "$target" == "$host_gnu" ]] || {
        echo "Error: gnu cross-builds are not supported here; build $target on a native host or use CI" >&2
        exit 1
      }
      if $DRY_RUN; then
        echo "+ JCODE_RELEASE_BUILD=1 JCODE_BUILD_SEMVER=$VERSION_NUM cargo build --release --target $target -p jcode --bin jcode"
        echo "+ cp <binary> $dist_dir/$artifact && tar czf $artifact.tar.gz"
      else
        (cd "$build_root" && \
          JCODE_RELEASE_BUILD=1 JCODE_BUILD_SEMVER="$VERSION_NUM" \
          CARGO_TARGET_DIR="$repo_root/target" \
          cargo build --release --target "$target" -p jcode --bin jcode)
        cp "$repo_root/target/$target/release/jcode" "$dist_dir/$artifact"
        chmod +x "$dist_dir/$artifact"
        (cd "$dist_dir" && tar czf "$artifact.tar.gz" "$artifact")
      fi
      ;;
  esac
done

# ---------------------------------------------------------------------------
# Checksums: SHA256SUMS carries one `<sha>  <asset>.tar.gz` line per asset.
# ---------------------------------------------------------------------------
if $DRY_RUN; then
  echo "+ (cd dist && sha256sum *.tar.gz > SHA256SUMS)"
else
  (cd "$dist_dir" && sha256sum ./*.tar.gz > SHA256SUMS)
  echo "SHA256SUMS:"
  cat "$dist_dir/SHA256SUMS"
fi

# ---------------------------------------------------------------------------
# Publish: versioned tag release (recreated on same-version repackage) and the
# fixed `latest` alias release (always recreated).
# ---------------------------------------------------------------------------
assets=("$dist_dir"/*.tar.gz "$dist_dir/SHA256SUMS")

publish() {
  local tag="$1" title="$2" notes="$3"
  if $DRY_RUN; then
    echo "+ gh api -X PATCH repos/$REPO/git/refs/tags/$tag -f sha=$ref_sha -F force=true"
    echo "+ gh release delete $tag --repo $REPO -y  (if exists)"
    echo "+ gh release create $tag --repo $REPO --target $ref_sha --title '$title' --notes '$notes'"
    echo "+ gh release upload $tag <${#assets[@]} assets> --repo $REPO --clobber"
    return 0
  fi
  # The tag always marks the packaged line's latest state: move it to the
  # built commit first so the recreated release's target_commitish follows —
  # installed builds detect a same-tag repackage by comparing it against
  # their embedded git hash.
  gh api -X PATCH "repos/$REPO/git/refs/tags/$tag" \
    -f sha="$ref_sha" -F force=true >/dev/null \
    || gh api -X POST "repos/$REPO/git/refs" \
         -f ref="refs/tags/$tag" -f sha="$ref_sha" >/dev/null
  gh release delete "$tag" --repo "$REPO" -y 2>/dev/null || true
  gh release create "$tag" --repo "$REPO" --target "$ref_sha" \
    --title "$title" --notes "$notes" --draft
  gh release upload "$tag" "${assets[@]}" --repo "$REPO" --clobber
  gh release edit "$tag" --repo "$REPO" --draft=false
}

publish "$TAG" "$TAG" "jcode $TAG (kmou424 fork)"

echo ""
echo "=== Fork release published ==="
echo "  release:   https://github.com/$REPO/releases/tag/$TAG"
echo "  machines update with: jcode update   (semver-max tag wins)"
echo "  rollback:  scripts/install.sh v<previous-tag>  (each version line keeps its own tag)"
