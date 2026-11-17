{
  description = "jorb dev shell";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";

    crane.url = "github:ipetkov/crane";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
    }:
    let
      inherit (nixpkgs) lib;
      defaultSystems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
      forAllSystems =
        fn:
        lib.genAttrs defaultSystems (
          system:
          let
            pkgs = import nixpkgs {
              inherit system;
              overlays = [ (import rust-overlay) ];
            };
          in
          fn pkgs
        );

      # Only the files a Rust build reads, so editing the plan or the progress
      # notes does not invalidate the checks.
      rustSource = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./clippy.toml
          ./jorb
          ./jorb-init
          ./jorb-vmm
        ];
      };

      rustToolchainFor = pkgs: pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

      # The workspace as crane sees it. `cargoArtifacts` builds the dependency
      # tree once so the test and clippy checks share it.
      workspaceFor =
        pkgs:
        let
          craneLib = (crane.mkLib pkgs).overrideToolchain (rustToolchainFor pkgs);
          # The workspace root is virtual, so crane has no package to read these
          # from.
          commonArgs = {
            pname = "jorb-workspace";
            version = "0.1.0";
            src = rustSource;
            strictDeps = true;
            CH_OPENAPI_SPEC = chOpenapiSpec pkgs;
          };
        in
        {
          inherit craneLib commonArgs;
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        };

      # The spec inside the source of the exact Cloud Hypervisor package the
      # shell installs, so the generated client cannot drift from the binary.
      chOpenapiSpec =
        pkgs: "${pkgs.cloud-hypervisor.src}/vmm/src/api/openapi/cloud-hypervisor.yaml";
    in
    {
      formatter = forAllSystems (pkgs: pkgs.nixfmt);

      devShells = forAllSystems (
        pkgs:
        let
          rustToolchain = rustToolchainFor pkgs;
        in
        {
          default = pkgs.mkShell {
            packages =
              [
                rustToolchain

                # scratch work and long-running host gates
                pkgs.tmux

                # base image (T6): pull, unpack, build the ext4, inspect it
                pkgs.skopeo
                pkgs.umoci
                pkgs.e2fsprogs
                pkgs.util-linux
                pkgs.coreutils

                # scripts and data wrangling in tests/host
                pkgs.jq
                pkgs.shellcheck
                pkgs.ripgrep
                pkgs.fd
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                # the VMM itself, plus the host-side pieces its gates touch
                pkgs.cloud-hypervisor
                pkgs.iproute2
                pkgs.nftables
                pkgs.xfsprogs
              ];

            # T2: build.rs generates the VMM client from this spec.
            CH_OPENAPI_SPEC = chOpenapiSpec pkgs;
          };
        }
      );

      # `nix flake check` runs what the Commands section of AGENTS.md lists:
      # formatting, clippy and the test suite, plus Nix evaluation.
      checks = forAllSystems (
        pkgs:
        let
          inherit (workspaceFor pkgs) craneLib commonArgs cargoArtifacts;
        in
        {
          test = craneLib.cargoTest (commonArgs // { inherit cargoArtifacts; });

          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );

          fmt = craneLib.cargoFmt { inherit (commonArgs) pname version src; };
        }
      );
    };
}
