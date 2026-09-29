{
  description = "jcode development shell";

  # A dev shell for working on jcode on NixOS: provides rustc/cargo with the
  # musl std and a musl cross C toolchain, matching what `selfdev build`
  # auto-detects (JCODE_SELFDEV_TARGET / /etc/NIXOS → musl target).
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

          # musl target + the nixpkgs cross stdenv whose bin/<triple>-gcc the
          # selfdev musl toolchain detection picks up.
          musl = {
            x86_64-linux = {
              triple = "x86_64-unknown-linux-musl";
              cc = pkgs.pkgsCross.musl64.buildPackages.gcc;
            };
            aarch64-linux = {
              triple = "aarch64-unknown-linux-musl";
              cc = pkgs.pkgsCross.aarch64-multiplatform-musl.buildPackages.gcc;
            };
          }.${system};

          rustToolchain = pkgs.rust-bin.stable.latest.default.override {
            targets = [ musl.triple ];
          };
        in
        {
          default = pkgs.mkShell {
            packages = [
              rustToolchain
              musl.cc
              pkgs.bashInteractive
              pkgs.git
              pkgs.pkg-config
              pkgs.python3
            ];
          };
        }
      );
    };
}
