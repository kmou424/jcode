#!/usr/bin/env bash
set -euo pipefail

# Build a statically-linked musl release binary. musl artifacts run everywhere
# (Alpine, NixOS, glibc distros) so they are the fork's universal Linux asset;
# the wrapper+payload split used by the glibc compat build is unnecessary
# because nothing is dynamically linked.
#
# Preferred path is a native cargo build with a musl C toolchain
# (<arch>-linux-musl-gcc cross prefix, or musl-gcc on a matching-arch host):
# it lands in the repo's normal target/ and cargo caches, so CI's rust-cache
# and local incremental caches apply across runs. When no musl toolchain is
# available the script falls back to a `rust:alpine` docker container.
#
# Usage: scripts/build_musl.sh <target> [out-dir]
#   target: x86_64-unknown-linux-musl | aarch64-unknown-linux-musl
#
# Cross-arch note: a matching *-linux-musl-gcc prefix enables cross builds
# without docker. Otherwise docker resolves the matching-arch image via
# --platform; on
# a foreign-arch host that needs binfmt_misc/qemu (docker's default setup on
# Docker Desktop and qemu-user-enabled Linux installs). The build still works
# through emulation, just slowly.

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

target="${1:-}"
out_dir="${2:-$repo_root/dist}"

case "$target" in
  x86_64-unknown-linux-musl)
    artifact="jcode-linux-musl-x86_64"
    platform="linux/amd64"
    ;;
  aarch64-unknown-linux-musl)
    artifact="jcode-linux-musl-aarch64"
    platform="linux/arm64"
    ;;
  *)
    echo "Usage: $0 <x86_64-unknown-linux-musl|aarch64-unknown-linux-musl> [out-dir]" >&2
    exit 1
    ;;
esac

if [[ "$out_dir" != /* ]]; then
  out_dir="$repo_root/$out_dir"
fi

image="${JCODE_MUSL_IMAGE:-rust:alpine}"
profile="${JCODE_MUSL_PROFILE:-release}"
cache_root="${JCODE_MUSL_CACHE_DIR:-$HOME/.cache/jcode-musl/$target}"

mkdir -p "$out_dir" \
  "$cache_root/cargo-registry" \
  "$cache_root/cargo-git" \
  "$cache_root/rustup"

host_uid="$(id -u)"
host_gid="$(id -g)"

# Compute git build metadata on the HOST and hand it to the container via a
# metadata file (read by jcode-build-meta/build.rs through
# JCODE_BUILD_METADATA_FILE). The repo is bind-mounted into the container and
# owned by the host UID while git inside the container runs as root, so any
# in-container `git` call trips git's "dubious ownership" guard
# (CVE-2022-24765) and fails. Same approach as scripts/build_linux_compat.sh.
git_hash=""
git_date=""
git_tag=""
git_dirty="0"
changelog_raw=""
if command -v git >/dev/null 2>&1 && git -C "$repo_root" rev-parse --git-dir >/dev/null 2>&1; then
  git_hash="$(git -C "$repo_root" rev-parse --short HEAD 2>/dev/null || true)"
  git_date="$(git -C "$repo_root" log -1 --format=%ci 2>/dev/null || true)"
  git_tag="$(git -C "$repo_root" describe --tags --always 2>/dev/null || true)"
  changelog_raw="$(git -C "$repo_root" log -700 --format='%h|%ct|%D|%s' 2>/dev/null || true)"
  if [[ -n "$(git -C "$repo_root" status --porcelain 2>/dev/null || true)" ]]; then
    git_dirty="1"
  fi
else
  echo "warning: git metadata unavailable on host; embedded changelog/version may be empty" >&2
fi

metadata_file="$(mktemp)"
trap 'rm -f "$metadata_file"' EXIT
{
  printf 'git_hash=%s\n' "$git_hash"
  printf 'git_date=%s\n' "$git_date"
  printf 'git_tag=%s\n' "$git_tag"
  printf 'git_dirty=%s\n' "$git_dirty"
  printf 'changelog_raw<<JCODE_CHANGELOG_EOF\n%s\nJCODE_CHANGELOG_EOF\n' "$changelog_raw"
} > "$metadata_file"

echo "Output dir: $out_dir"
echo "Embedding git metadata: hash=${git_hash:-<none>} tag=${git_tag:-<none>} dirty=$git_dirty"

host_arch="$(uname -m)"
[[ "$host_arch" = "arm64" ]] && host_arch="aarch64"
target_arch="${target%%-*}"

# A real musl C toolchain on the host lets cargo build directly: the
# workspace's normal target/ dir and cargo caches are used, so incremental
# and rust-cache reuse applies. `musl-gcc` is only valid when the host arch
# matches the target arch; a prefixed <arch>-linux-musl-gcc is always usable.
musl_cc=""
if command -v "${target_arch}-linux-musl-gcc" >/dev/null 2>&1; then
  musl_cc="${target_arch}-linux-musl-gcc"
elif [[ "$host_arch" = "$target_arch" ]] && command -v musl-gcc >/dev/null 2>&1; then
  musl_cc="musl-gcc"
fi

if [[ -n "$musl_cc" ]]; then
  echo "Building musl static release for $target natively (CC: $musl_cc, linker: rust-lld)"
  cc_var="$(printf 'CC_%s' "$target" | tr 'a-z-' 'A-Z_')"
  # Do NOT override CARGO_TARGET_<T>_LINKER: rustc's default rust-lld for musl
  # targets links musl libc statically (self-contained mode). Routing the
  # final link through a gcc driver emits a dynamic ELF that still needs
  # /lib/ld-musl-*.so.1 — musl-gcc is only for compiling C dependencies.
  export JCODE_RELEASE_BUILD="${JCODE_RELEASE_BUILD:-1}" \
    JCODE_BUILD_SEMVER="${JCODE_BUILD_SEMVER:-}" \
    JCODE_BUILD_METADATA_FILE="$metadata_file" \
    JCODE_BUILD_GIT_HASH="$git_hash" \
    JCODE_BUILD_GIT_DATE="$git_date" \
    JCODE_BUILD_GIT_TAG="$git_tag" \
    JCODE_BUILD_GIT_DIRTY="$git_dirty" \
    CC="$musl_cc" TARGET_CC="$musl_cc" "$cc_var"="$musl_cc"
  rustup target add "$target" 2>/dev/null || true
  cargo build --profile "$profile" --target "$target" \
    -p jcode --bin jcode --features linux-compat-vendored-openssl \
    --manifest-path "$repo_root/Cargo.toml"
  cp "$repo_root/target/$target/$profile/jcode" "$out_dir/$artifact"
  chmod +x "$out_dir/$artifact"
  (cd "$out_dir" && tar czf "$artifact.tar.gz" "$artifact")
else
echo "Building musl static release for $target in Docker image: $image (platform $platform)"
docker run --rm --platform "$platform" \
  -e CARGO_TERM_COLOR=always \
  -e JCODE_RELEASE_BUILD="${JCODE_RELEASE_BUILD:-1}" \
  -e JCODE_BUILD_SEMVER="${JCODE_BUILD_SEMVER:-}" \
  -e JCODE_BUILD_METADATA_FILE=/jcode-build-meta \
  -e JCODE_BUILD_GIT_HASH="$git_hash" \
  -e JCODE_BUILD_GIT_DATE="$git_date" \
  -e JCODE_BUILD_GIT_TAG="$git_tag" \
  -e JCODE_BUILD_GIT_DIRTY="$git_dirty" \
  -e HOST_UID="$host_uid" \
  -e HOST_GID="$host_gid" \
  -e MUSL_TARGET="$target" \
  -e MUSL_PROFILE="$profile" \
  -v "$repo_root:/work" \
  -v "$metadata_file:/jcode-build-meta:ro" \
  -v "$out_dir:/out" \
  -v "$cache_root/cargo-registry:/root/.cargo/registry" \
  -v "$cache_root/cargo-git:/root/.cargo/git" \
  -v "$cache_root/rustup:/root/.rustup" \
  -w /work \
  "$image" \
  sh -c '
    set -euo pipefail
    # `sh -c` keeps the docker ENV PATH; a login shell would drop
    # /usr/local/cargo/bin via /etc/profile and lose rustup/cargo.
    export PATH="/usr/local/cargo/bin:$PATH"
    apk add --no-cache build-base ca-certificates cmake git perl pkgconfig
    # git deps (agentgrep, mermaid-rs-renderer) are fetched over https.
    git config --global --add safe.directory /work 2>/dev/null || true
    rustup target add "$MUSL_TARGET"

    # Alpine'\''s gcc already targets musl; point cc-rs at it explicitly so
    # C dependencies (aws-lc-sys, ring, onig_sys, vendored openssl) do not
    # hunt for a nonexistent *-linux-musl-gcc prefixed toolchain.
    cc_var="$(printf "CC_%s" "$MUSL_TARGET" | tr "a-z-" "A-Z_")"
    export "$cc_var"=gcc

    export CARGO_TARGET_DIR=/work/target/musl
    cargo build --profile "$MUSL_PROFILE" --target "$MUSL_TARGET" \
      -p jcode --bin jcode --features linux-compat-vendored-openssl

    cp "$CARGO_TARGET_DIR/$MUSL_TARGET/$MUSL_PROFILE/jcode" "/out/'"$artifact"'"
    chmod +x "/out/'"$artifact"'"
    (cd /out && tar czf '"$artifact"'.tar.gz '"$artifact"')
    chown "$HOST_UID:$HOST_GID" "/out/'"$artifact"'" "/out/'"$artifact"'.tar.gz" 2>/dev/null || true
  '
fi

for required in "$out_dir/$artifact" "$out_dir/$artifact.tar.gz"; do
  if [[ ! -s "$required" ]]; then
    echo "error: build did not produce $required" >&2
    exit 1
  fi
done

echo "Built artifacts:"
ls -lh "$out_dir/$artifact" "$out_dir/$artifact.tar.gz"

# A musl artifact must be fully static; a dynamic binary would still fail on
# NixOS/Alpine and silently defeat the point of this build. rust-lld emits a
# static-PIE on x86_64 ("static-pie linked") and a plain static exe on
# aarch64 ("statically linked") — both have no dynamic section; a gcc-driver
# link would say "dynamically linked" plus an interpreter and must fail here.
if command -v file >/dev/null 2>&1; then
  if ! file "$out_dir/$artifact" | grep -qE "statically linked|static-pie linked"; then
    echo "error: $artifact is not statically linked:" >&2
    file "$out_dir/$artifact" >&2
    exit 1
  fi
fi

# Fail closed when the embedded hash does not match the tree that was built
# (stale build-cache protection, same as build_linux_compat.sh). Runs through
# binfmt emulation when the artifact is foreign-arch.
if [[ -n "$git_hash" ]]; then
  embedded="$("$out_dir/$artifact" --no-update --no-selfdev version 2>/dev/null \
    | awk -F'\t' '$1 == "version" { print $2; exit }')"
  if [[ -n "$embedded" && "$embedded" != *"$git_hash"* ]]; then
    echo "error: embedded build metadata reports '$embedded' but the tree is at '$git_hash'" >&2
    echo "       (stale build cache; re-run after 'cargo clean' or bump JCODE_BUILD_GIT_HASH)" >&2
    exit 1
  fi
fi
