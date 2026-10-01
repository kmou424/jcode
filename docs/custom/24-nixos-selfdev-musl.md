# [24] NixOS selfdev musl builds

## What it does

`selfdev build` (tool action, `jcode self-dev --build`, and every other
caller of `selfdev_build_command*`) automatically produces a static musl
binary when running on NixOS: `/etc/NIXOS` marks the install, and NixOS has
no FHS glibc loader, so a normally-linked dev binary would not run where it
was built. The binary lands in `target/<triple>/selfdev/jcode` and every
consumer (`find_dev_binary`, `selfdev_binary_path`,
`client_update_candidate`, publish/install) resolves the same path.

Detection is `selfdev_cargo_target_triple()`:

1. `JCODE_SELFDEV_TARGET` set → that triple; an empty/whitespace value
   explicitly forces the host default (escape hatch without a flag).
2. `/etc/NIXOS` exists → `<host-arch>-unknown-linux-musl`.
3. Otherwise `None` — unchanged host-default build.

When the effective triple is musl the build command gains
`--target <triple> --features linux-compat-vendored-openssl` (there is no
system OpenSSL to link a static musl binary against) and, if a musl C
toolchain is found, exports `CC`, `TARGET_CC`, and `CC_<TRIPLE>` pointing at
it. Candidates probed in order: `musl-gcc` (only when target arch equals
host arch — it always emits host-arch objects), `<arch>-linux-musl-gcc`
(distro cross prefix, matches `scripts/build_musl.sh`), `<triple>-gcc`
(nixpkgs cross stdenv, e.g. `x86_64-unknown-linux-musl-gcc`). No toolchain
found leaves Cargo's defaults in place so the compiler error names what is
missing. Non-musl explicit triples get only `--target`.

`SelfDevBuildCommand` gained `env: Vec<(String, String)>`
(`#[serde(default)]`), applied by both executors: `run_selfdev_build`
(`jcode self-dev --build`) and the selfdev tool's
`stream_build_command` build queue.

`flake.nix` provides the matching devShell: `nix develop` supplies a
rust-overlay toolchain with the musl std target and the nixpkgs musl cross
gcc (`pkgsCross.musl64` on x86_64, `pkgsCross.aarch64-multiplatform-musl` on
aarch64) — no rustup needed on NixOS.

## Code surface

- `crates/jcode-build-support/src/paths.rs`: `nixos_host`,
  `selfdev_cargo_target_triple(+_with)`, `is_musl_triple`,
  `selfdev_target_args/_suffix`, `cc_env_var`, `musl_cc_candidates`,
  `detect_musl_cc`, `selfdev_build_env`,
  `selfdev_binary_path_for_target`; the three `SelfDevBuildCommand`
  constructors gain `env`, and `cargo_build_args`/`display_build_command`
  take the target suffix.
- `crates/jcode-selfdev-types/src/lib.rs`: `SelfDevBuildCommand.env`.
- `crates/jcode-app-core/src/tool/selfdev/build_queue.rs`: envs applied in
  `stream_build_command`; test-mode literal gets `env: Vec::new()`.
- `flake.nix`: devShell (rust-overlay + musl target + musl cross gcc).

## Upstream status

Upstream-candidate: NixOS selfdev support helps any NixOS contributor; the
`/etc/NIXOS` probe and `JCODE_SELFDEV_TARGET` override carry no
fork-specific state. `flake.nix` likewise.
