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
      # jorb supports one platform: aarch64 NixOS.
      defaultSystems = [ "aarch64-linux" ];
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
          ./jorb-config-disk
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
      chOpenapiSpec = pkgs: "${pkgs.cloud-hypervisor.src}/vmm/src/api/openapi/cloud-hypervisor.yaml";

      # The job runner itself: one invocation, one job.
      jorbFor =
        pkgs:
        let
          inherit (workspaceFor pkgs) craneLib commonArgs cargoArtifacts;
        in
        craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "jorb";
            cargoExtraArgs = "--package jorb";
            doCheck = false;
            meta.platforms = defaultSystems;
          }
        );

      # jorb-init is PID 1 in a guest with no loader and no libraries, so it is
      # built against musl and statically linked. The guest runs the host's
      # architecture, so this is not a cross-build in any real sense.
      jorbInitFor =
        pkgs:
        let
          craneLib = (crane.mkLib pkgs).overrideToolchain (rustToolchainFor pkgs);
          target = "${pkgs.stdenv.hostPlatform.parsed.cpu.name}-unknown-linux-musl";
          linkerVariable = "CARGO_TARGET_${
            lib.toUpper (builtins.replaceStrings [ "-" ] [ "_" ] target)
          }_LINKER";
          musl = pkgs.pkgsMusl.stdenv.cc;
        in
        craneLib.buildPackage {
          pname = "jorb-init";
          version = "0.1.0";
          src = rustSource;
          strictDeps = true;

          cargoExtraArgs = "--package jorb-init";
          CARGO_BUILD_TARGET = target;
          CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
          ${linkerVariable} = "${musl.targetPrefix}cc";
          nativeBuildInputs = [ musl ];

          # The workspace's tests run in the `test` check, on the host target.
          doCheck = false;

          meta.platforms = defaultSystems;
        };
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
            packages = [
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
              pkgs.curl
              pkgs.file
              pkgs.jq
              pkgs.shellcheck
              pkgs.ripgrep
              pkgs.fd

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

      packages = forAllSystems (pkgs: {
        default = jorbFor pkgs;
        jorb = jorbFor pkgs;
        guest-kernel = pkgs.callPackage ./nix/kernel.nix { };
        test-image = pkgs.callPackage ./tests/image/image.nix { jorb-init = jorbInitFor pkgs; };
        jorb-init = jorbInitFor pkgs;
        jorb-prepare-image = pkgs.callPackage ./nix/image.nix { jorb-init = jorbInitFor pkgs; };
        # The VMM the client is generated for, so the module runs that one.
        inherit (pkgs) cloud-hypervisor;
      });

      # The host side, with this flake's own builds as the defaults the module
      # would otherwise have no way to name.
      nixosModules.default =
        { pkgs, ... }:
        let
          ours = self.packages.${pkgs.stdenv.hostPlatform.system};
        in
        {
          imports = [ ./nix/module.nix ];

          services.jorb = {
            package = lib.mkDefault ours.jorb;
            prepareImagePackage = lib.mkDefault ours.jorb-prepare-image;
            kernel = lib.mkDefault "${ours.guest-kernel}/kernel";
            cloudHypervisorPackage = lib.mkDefault ours.cloud-hypervisor;
          };
        };

      nixosModules.jorb = self.nixosModules.default;

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

          # Evaluates a host that enables the module, then checks the network
          # policy it generated. `nft -c` needs a netlink socket, which a
          # private network namespace provides without any privileges.
          module =
            let
              host = nixpkgs.lib.nixosSystem {
                inherit (pkgs.stdenv.hostPlatform) system;
                modules = [
                  self.nixosModules.default
                  {
                    services.jorb = {
                      enable = true;
                      tokenFile = "/var/lib/secrets/jorb-provider.env";
                      memoryHigh = "11797M";
                      observability.enable = true;
                      # Next to the directory it mounts on, not inside it.
                      stateImage = "/var/lib/jorb.img";
                    };

                    # Enough of a host to evaluate, not to boot.
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

              # The module's table as nft reads it. `networking.nftables.ruleset`
              # holds only free-form rules, not the tables, so checking it
              # checked nothing.
              policy =
                let
                  table = host.config.networking.nftables.tables.jorb;
                in
                "table ${table.family} ${table.name} {\n${table.content}\n}\n";

              # The parts of the configuration this check is about, forced by
              # being written down.
              evaluated = pkgs.writeTextDir "evidence" ''
                ${host.config.systemd.units."jorb-provider.service".text}
                ${host.config.systemd.units."jorb-base-image.service".text}
                ${host.config.systemd.units."jorb-state-image.service".text}
                ${host.config.environment.etc.fstab.text}
                ${host.config.systemd.units."jorb.slice".text}
                ${host.config.environment.etc.sudoers.source}
                net.ipv4.ip_forward = ${toString host.config.boot.kernel.sysctl."net.ipv4.ip_forward"}
                zramSwap.enable = ${lib.boolToString host.config.zramSwap.enable}
              '';
            in
            # The VMM the host runs is the one whose spec generated the client.
            assert lib.assertMsg (
              "${host.config.services.jorb.cloudHypervisorPackage.src}" == "${pkgs.cloud-hypervisor.src}"
            ) "the module's Cloud Hypervisor is not the one jorb-vmm was generated from";
            pkgs.runCommand "jorb-module"
              {
                nativeBuildInputs = [
                  pkgs.jq
                  pkgs.nftables
                  pkgs.util-linux
                ];
              }
              ''
                cp ${pkgs.writeText "jorb.nft" policy} ruleset.nft
                grep -q 'iifname "jorb\*" drop' ruleset.nft
                unshare --map-root-user --net nft --check --file ruleset.nft

                # The dashboard has to be JSON Grafana will take, and every
                # panel has to name a query.
                jq -e '.panels | length > 0' ${./nix/jorb-dashboard.json} >/dev/null
                jq -e '
                  [.panels[] | select(.type != "row")]
                  | all(.targets | length > 0 and all(.expr | length > 0))
                ' ${./nix/jorb-dashboard.json} >/dev/null

                # The provider must wait for the base image, and the state
                # image must be mounted on the state directory after the unit
                # that creates it.
                # The one sudo rule: the provider user, as root only, without
                # SETENV, which would let the caller set any variable.
                grep -q '^jorb[[:space:]]*ALL=(root)[[:space:]]*NOPASSWD: /nix/store/[^ ]*-jorb-root/bin/jorb-root$' \
                  "${evaluated}/evidence"
                if grep -q SETENV "${evaluated}/evidence"; then
                  echo "the sudo rule carries SETENV" >&2
                  exit 1
                fi

                grep -q 'Requires=jorb-base-image.service' "${evaluated}/evidence"
                grep -q '/var/lib/jorb.img /var/lib/jorb xfs .*x-systemd.requires=jorb-state-image.service' \
                  "${evaluated}/evidence"

                mkdir -p "$out"
                cat ${evaluated}/evidence > "$out/evidence"
                cat ${host.config.environment.etc.sudoers.source} >> "$out/evidence"
                cat ${host.config.environment.etc."alloy/jorb.alloy".source} > "$out/jorb.alloy"
              '';
        }
      );
    };
}
