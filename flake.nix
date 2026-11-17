{
  description = "jorb dev shell";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
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
    in
    {
      formatter = forAllSystems (pkgs: pkgs.nixfmt);

      devShells = forAllSystems (
        pkgs:
        let
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
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
            CH_OPENAPI_SPEC = "${pkgs.cloud-hypervisor.src}/vmm/src/api/openapi/cloud-hypervisor.yaml";
          };
        }
      );

      # `nix flake check` runs what the Commands section of AGENTS.md lists:
      # formatting, clippy and the test suite, plus Nix evaluation.
      checks = forAllSystems (
        pkgs:
        let
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          workspace = rustPlatform.buildRustPackage {
            pname = "jorb-workspace";
            version = "0.1.0";
            src = rustSource;
            cargoLock.lockFile = ./Cargo.lock;
            CH_OPENAPI_SPEC = "${pkgs.cloud-hypervisor.src}/vmm/src/api/openapi/cloud-hypervisor.yaml";
          };
        in
        {
          # Builds the workspace and runs `cargo test` in the check phase.
          test = workspace;

          clippy = workspace.overrideAttrs (previous: {
            pname = "jorb-clippy";
            nativeBuildInputs = previous.nativeBuildInputs ++ [ rustToolchain ];
            buildPhase = "cargo clippy --offline --workspace --all-targets -- -D warnings";
            doCheck = false;
            installPhase = "touch $out";
          });

          fmt = pkgs.runCommand "jorb-fmt" { nativeBuildInputs = [ rustToolchain ]; } ''
            cd ${rustSource}
            cargo fmt --all --check
            touch $out
          '';
        }
      );
    };
}
