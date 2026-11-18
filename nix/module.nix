# The host side of jorb: the provider, the one sudo rule it goes through, the
# slice the orbs live in, the base image, and the network policy.
#
# The provider is a closed-source, self-updating CLI running unprivileged. It
# may ask for an operation on an orb and nothing else: the sudo rule carries
# four variables across, and the root side sets everything about the host.
packages:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.jorb;
  ours = packages.${pkgs.stdenv.hostPlatform.system};

  stateDirectory = "/var/lib/jorb";
  runDirectory = "/run/jorb";
  preserved = [
    "ORB_OPERATION"
    "ORB_ID"
    "ORB_TOKEN"
    "AMP_URL"
  ];

  # The only program the sudo rule allows. It unsets everything but the
  # preserved variables (in the shell, so the token is in no argv), then sets
  # the host's settings itself.
  rootSide = pkgs.writeShellApplication {
    name = "jorb-root";
    text = ''
      for name in $(compgen -e); do
        case $name in
        ${lib.concatStringsSep " | " preserved}) ;;
        *) unset "$name" ;;
        esac
      done

      export PATH=${
        lib.makeBinPath [
          config.systemd.package
          pkgs.nftables
        ]
      }
      export JORB_SLOTS=${toString cfg.slots}
      export JORB_VCPUS=${toString cfg.vcpus}
      export JORB_MEMORY_MIB=${toString cfg.memoryMiB}
      export JORB_KERNEL=${ours.guest-kernel}/kernel
      export JORB_STATE_DIR=${stateDirectory}
      export JORB_RUN_DIR=${runDirectory}
      export JORB_CH_BIN=${ours.cloud-hypervisor}/bin/cloud-hypervisor
      exec ${ours.jorb}/bin/jorb
    '';
  };

  # What `amp orb provider` runs. The setuid sudo, by path: the store's has no
  # setuid bit. -n, because nobody is there to type a password.
  dispatch = pkgs.writeShellScript "jorb-dispatch" ''
    exec ${config.security.wrapperDir}/sudo -n ${rootSide}/bin/jorb-root
  '';
in

{
  options.services.jorb = with lib; {
    enable = mkEnableOption "the jorb Amp orb provider";

    slots = mkOption {
      type = types.ints.between 1 256;
      default = 1;
      description = "How many orbs may run at once.";
    };

    vcpus = mkOption {
      type = types.ints.positive;
      default = 4;
    };

    memoryMiB = mkOption {
      type = types.ints.positive;
      default = 6144;
      description = "Guest memory per orb; freed pages go back to the host.";
    };

    memoryHigh = mkOption {
      type = types.str;
      default = "75%";
      description = "`MemoryHigh` for `jorb.slice`: past it, orbs are throttled into zram.";
    };

    tokenFile = mkOption {
      # Not types.path: a path literal would copy the credential into the store.
      type = types.str;
      description = "Environment file with the provider's own Amp credential.";
    };

    stateImage = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/var/lib/jorb.img";
      description = ''
        A sparse 128 GiB XFS image to create and loop-mount on ${stateDirectory},
        for a host whose filesystem cannot reflink.
      '';
    };

    observability = {
      enable = mkEnableOption "jorb's Alloy fragment and Grafana dashboard";
      metricsEndpoint = mkOption {
        type = types.str;
        default = "http://localhost:9090/api/v1/write";
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        assertions = [
          {
            assertion = lib.hasPrefix "/" cfg.tokenFile;
            message = "services.jorb.tokenFile must be an absolute path outside the Nix store.";
          }
          {
            assertion = cfg.stateImage == null || !lib.hasPrefix "${stateDirectory}/" cfg.stateImage;
            message = "services.jorb.stateImage must be outside ${stateDirectory}, or the mount hides it.";
          }
          {
            assertion = config.networking.nftables.enable;
            message = "services.jorb needs nftables: jorb starts no orb without its policy table.";
          }
          {
            assertion = toString config.boot.kernel.sysctl."net.ipv4.ip_forward" == "1";
            message = "services.jorb needs net.ipv4.ip_forward: orbs reach the internet through the host.";
          }
        ];

        # Defaults, because each reaches beyond jorb: zram is host-wide swap,
        # and forwarding routes between all the host's interfaces.
        zramSwap.enable = lib.mkDefault true;
        boot.kernel.sysctl."net.ipv4.ip_forward" = lib.mkDefault 1;
        networking.nftables.enable = lib.mkDefault true;

        users.users.jorb = {
          isSystemUser = true;
          group = "jorb";
        };
        users.groups.jorb = { };

        # No SETENV: with it, variables on the sudo command line bypass
        # env_keep, and the caller could pick LD_PRELOAD or the VMM.
        security.sudo.extraRules = [
          {
            users = [ "jorb" ];
            runAs = "root";
            commands = [
              {
                command = "${rootSide}/bin/jorb-root";
                options = [ "NOPASSWD" ];
              }
            ];
          }
        ];
        security.sudo.extraConfig = ''
          Defaults:jorb env_keep += "${lib.concatStringsSep " " preserved}"
        '';

        # In system.slice, so jorb.slice's MemoryHigh bounds only the orbs.
        systemd.services.jorb-provider = {
          wantedBy = [ "multi-user.target" ];
          after = [
            "network-online.target"
            "jorb-base-image.service"
          ];
          wants = [ "network-online.target" ];
          requires = [ "jorb-base-image.service" ];
          unitConfig.RequiresMountsFor = [ stateDirectory ];
          serviceConfig = {
            ExecStart = "${pkgs.amp-cli}/bin/amp orb provider ${dispatch}";
            EnvironmentFile = cfg.tokenFile;
            User = "jorb";
            Group = "jorb";
            Slice = "system.slice";
            Restart = "always";
            RestartSec = 5;
          };
        };

        # The pull takes minutes and needs the network, so it retries until it
        # works and the provider waits for it.
        systemd.services.jorb-base-image = {
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          unitConfig.RequiresMountsFor = [ stateDirectory ];
          startLimitIntervalSec = 0;
          environment.JORB_STATE_DIR = stateDirectory;
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            ExecStart = "${ours.jorb-prepare-image}/bin/jorb-prepare-image";
            TimeoutStartSec = "30min";
            Restart = "on-failure";
            RestartSec = 30;
          };
        };

        systemd.slices.jorb.sliceConfig = {
          MemoryHigh = cfg.memoryHigh;
          MemoryAccounting = true;
          CPUAccounting = true;
          IOAccounting = true;
        };

        # 0700: a config disk holds a token.
        systemd.tmpfiles.rules = [
          "d ${stateDirectory} 0700 root root -"
          "d ${stateDirectory}/disks 0700 root root -"
          "d ${runDirectory} 0700 root root -"
        ];

        # Orbs are internet-only: nothing they send reaches the host, a
        # non-public address, or anything over IPv6, and nothing reaches them
        # that they did not ask for.
        networking.nftables.tables.jorb = {
          family = "inet";
          content = ''
            set not-the-internet {
              type ipv4_addr
              flags interval
              elements = {
                0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16,
                172.16.0.0/12, 192.0.0.0/24, 192.0.2.0/24, 192.168.0.0/16,
                198.18.0.0/15, 198.51.100.0/24, 203.0.113.0/24, 224.0.0.0/4, 240.0.0.0/4
              }
            }

            # Each tap and the one address its guest may send from, so an orb
            # cannot take another's address and receive its replies.
            set slot-sources {
              type ifname . ipv4_addr
              elements = { ${
                lib.concatMapStringsSep ", " (slot: ''"jorb${toString slot}" . 10.77.${toString slot}.2'') (
                  lib.range 0 (cfg.slots - 1)
                )
              } }
            }

            chain input {
              type filter hook input priority filter; policy accept;
              iifname "jorb*" drop
            }

            chain forward {
              type filter hook forward priority filter; policy accept;
              iifname "jorb*" meta nfproto ipv6 drop
              iifname "jorb*" iifname . ip saddr != @slot-sources drop
              iifname "jorb*" ip daddr @not-the-internet drop
              oifname "jorb*" ct state != { established, related } drop
            }

            # Host processes too: nothing on the host opens a connection into an orb.
            chain output {
              type filter hook output priority filter; policy accept;
              oifname "jorb*" ct state != { established, related } drop
            }

            chain postrouting {
              type nat hook postrouting priority srcnat; policy accept;
              ip saddr 10.77.0.0/16 oifname != "jorb*" masquerade
            }
          '';
        };
      }

      (lib.mkIf (cfg.stateImage != null) {
        # Formatted under a temporary name and renamed, so a kill leaves no
        # image or a whole one.
        systemd.services.jorb-state-image = {
          unitConfig = {
            DefaultDependencies = false;
            RequiresMountsFor = [ (dirOf cfg.stateImage) ];
          };
          path = [ pkgs.xfsprogs ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
          };
          script = ''
            image=${lib.escapeShellArg cfg.stateImage}
            [ -e "$image" ] && exit 0
            rm -f "$image.partial"
            truncate -s 128G "$image.partial"
            mkfs.xfs -q -L jorb "$image.partial"
            sync "$image.partial"
            mv "$image.partial" "$image"
            sync "$(dirname "$image")"
          '';
        };

        # Before tmpfiles, so the directories land on the mount, not under it.
        fileSystems.${stateDirectory} = {
          device = cfg.stateImage;
          fsType = "xfs";
          options = [
            "loop"
            "nofail"
            "discard"
            "x-systemd.requires=jorb-state-image.service"
            "x-systemd.before=systemd-tmpfiles-setup.service"
          ];
        };
      })

      (lib.mkIf cfg.observability.enable {
        environment.etc."alloy/jorb.alloy".source = pkgs.replaceVars ./jorb.alloy {
          JORB_METRICS_ENDPOINT = cfg.observability.metricsEndpoint;
        };
        services.grafana.provision.dashboards.settings.providers = [
          {
            name = "jorb";
            options.path = pkgs.linkFarm "jorb-dashboards" [
              {
                name = "jorb.json";
                path = ./jorb-dashboard.json;
              }
            ];
          }
        ];
      })
    ]
  );
}
