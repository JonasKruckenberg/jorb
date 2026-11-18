{
  description = "jorb: a self-hosted Amp orb provider on Cloud Hypervisor";

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
      system = "aarch64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };
      craneLib = (crane.mkLib pkgs).overrideToolchain (
        pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
      );

      # jorb-vmm's client is generated from the spec in the source of the very
      # Cloud Hypervisor the module runs, so the two cannot drift.
      chOpenapiSpec = "${pkgs.cloud-hypervisor.src}/vmm/src/api/openapi/cloud-hypervisor.yaml";

      # Only what a Rust build reads, so editing the notes rebuilds nothing.
      common = {
        pname = "jorb-workspace";
        version = "0.1.0";
        strictDeps = true;
        CH_OPENAPI_SPEC = chOpenapiSpec;
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./clippy.toml
            ./jorb
            ./jorb-config-disk
            ./jorb-init
            ./jorb-vmm
          ];
        };
      };
      cargoArtifacts = craneLib.buildDepsOnly common;

      jorb = craneLib.buildPackage (
        common
        // {
          inherit cargoArtifacts;
          pname = "jorb";
          cargoExtraArgs = "--package jorb";
          doCheck = false;
        }
      );

      # PID 1 in a guest with no loader and no libraries: static musl.
      jorb-init = craneLib.buildPackage (
        common
        // {
          pname = "jorb-init";
          cargoExtraArgs = "--package jorb-init";
          CARGO_BUILD_TARGET = "aarch64-unknown-linux-musl";
          CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsMusl.stdenv.cc.targetPrefix}cc";
          nativeBuildInputs = [ pkgs.pkgsMusl.stdenv.cc ];
          doCheck = false;
        }
      );

      # A host that enables the module: enough to evaluate, not to boot.
      host = lib.nixosSystem {
        inherit system;
        modules = [
          self.nixosModules.default
          {
            services.jorb = {
              enable = true;
              tokenFile = "/var/lib/secrets/jorb-provider.env";
              observability.enable = true;
              stateImage = "/var/lib/jorb.img";
              slots = 2;
            };
            boot.loader.grub.enable = false;
            fileSystems."/" = {
              device = "/dev/null";
              fsType = "ext4";
            };
            nixpkgs.config.allowUnfreePredicate = package: lib.getName package == "amp-cli";
            system.stateVersion = lib.versions.majorMinor lib.version;
          }
        ];
      };
      policy =
        let
          table = host.config.networking.nftables.tables.jorb;
        in
        pkgs.writeText "jorb.nft" "table ${table.family} ${table.name} {\n${table.content}\n}\n";
    in
    {
      formatter.${system} = pkgs.nixfmt;

      packages.${system} = {
        default = jorb;
        inherit jorb jorb-init;
        inherit (pkgs) cloud-hypervisor;
        guest-kernel = pkgs.callPackage ./nix/kernel.nix { };
        jorb-prepare-image = pkgs.callPackage ./nix/image.nix { inherit jorb-init; };
        test-image = pkgs.callPackage ./tests/image/image.nix { inherit jorb-init; };
      };

      nixosModules.default = import ./nix/module.nix self.packages;

      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
          cloud-hypervisor
          curl
          e2fsprogs
          iproute2
          jq
          nftables
          shellcheck
          skopeo
          umoci
        ];
        CH_OPENAPI_SPEC = chOpenapiSpec;
      };

      checks.${system} = {
        test = craneLib.cargoTest (common // { inherit cargoArtifacts; });
        clippy = craneLib.cargoClippy (
          common
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          }
        );
        fmt = craneLib.cargoFmt common;

        # The toplevel's path forces the whole evaluation, which is what runs
        # the NixOS assertions. `nft -c` needs only a private network namespace.
        module =
          pkgs.runCommand "jorb-module"
            {
              nativeBuildInputs = with pkgs; [
                jq
                nftables
                util-linux
              ];
              toplevel = builtins.unsafeDiscardStringContext host.config.system.build.toplevel.drvPath;
            }
            ''
              unshare --map-root-user --net nft --check --file ${policy}
              jq -e '[.panels[] | select(.type != "row")] | length > 0 and all(.targets | length > 0)' \
                ${./nix/jorb-dashboard.json} >/dev/null
              sudoers=${host.config.environment.etc.sudoers.source}
              grep -q '^jorb[[:space:]]*ALL=(root)[[:space:]]*NOPASSWD: /nix/store/[^ ]*/bin/jorb-root$' "$sudoers"
              if grep -q SETENV "$sudoers"; then exit 1; fi
              touch "$out"
            '';
      };
    };
}
