# The host side of jorb: the provider service, the one sudo rule it goes
# through, the slice the orbs live in, and the network policy.
#
# The privilege split is the point of this file. The provider is a
# closed-source, self-updating CLI running as an unprivileged user; it may ask
# for an operation on an orb and nothing else. The sudo rule carries exactly
# four variables across, and the root side sets every JORB_* setting itself,
# so the caller cannot describe the host.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.jorb;

  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    types
    ;

  # What the provider is allowed to say. Everything else about a job is the
  # host's business, and the root side below decides it.
  preserved = [
    "ORB_OPERATION"
    "ORB_ID"
    "ORB_TOKEN"
    "AMP_URL"
  ];

  # The host as jorb is told about it, and the only programs jorb runs. The
  # VMM is an absolute store path so nothing on the caller's side picks it.
  hostEnvironment = {
    PATH = lib.makeBinPath [
      config.systemd.package
      pkgs.nftables
    ];
    JORB_CH_BIN = "${cfg.cloudHypervisorPackage}/bin/cloud-hypervisor";
    JORB_SLOTS = toString cfg.slots;
    JORB_VCPUS = toString cfg.vcpus;
    JORB_MEMORY_MIB = toString cfg.memoryMiB;
    JORB_KERNEL = "${cfg.kernel}";
    JORB_STATE_DIR = cfg.stateDirectory;
    JORB_RUN_DIR = cfg.runtimeDirectory;
  };

  # The root side: the only program the sudo rule allows. It takes no
  # arguments and passes none on.
  #
  # jorb starts from an environment holding the four preserved variables and
  # the host's own settings, nothing else. Unsetting in the shell rather than
  # `env -i NAME=VALUE` keeps ORB_TOKEN out of every argv.
  rootSide = pkgs.writeShellApplication {
    name = "jorb-root";
    text = ''
      for name in $(compgen -e); do
        case $name in
        ${lib.concatStringsSep " | " preserved}) ;;
        *) unset "$name" ;;
        esac
      done

      ${lib.concatStringsSep "\n" (
        lib.mapAttrsToList (name: value: "export ${name}=${lib.escapeShellArg value}") hostEnvironment
      )}

      exec ${cfg.package}/bin/jorb
    '';
  };

  # What `amp orb provider` runs, as the unprivileged provider user.
  dispatch = pkgs.writeShellApplication {
    name = "jorb-dispatch";
    text = ''
      # The setuid wrapper: the store's sudo has no setuid bit. -n so a
      # missing sudo rule fails rather than waits for a password that no one
      # is there to type.
      exec ${config.security.wrapperDir}/sudo -n ${rootSide}/bin/jorb-root
    '';
  };
in

{
  options.services.jorb = {
    enable = mkEnableOption "the jorb Amp orb provider";

    slots = mkOption {
      type = types.ints.between 1 256;
      default = 1;
      description = ''
        How many orbs may run at once. Each slot is one tap and one
        `10.77.S.0/30`, and an orb holds its slot while its VM runs.
      '';
    };

    vcpus = mkOption {
      type = types.ints.positive;
      default = 4;
      description = "vCPUs per orb.";
    };

    memoryMiB = mkOption {
      type = types.ints.positive;
      default = 6144;
      description = ''
        Nominal guest memory per orb. The guest gives back what it frees, so
        this is a ceiling rather than a reservation.
      '';
    };

    memoryHigh = mkOption {
      type = types.str;
      default = "75%";
      example = "11797M";
      description = ''
        `MemoryHigh` for `jorb.slice`, which is the host's real protection:
        past it the orbs are throttled and reclaimed into zram rather than
        taking the host down. For an absolute figure, read `MemAvailable`
        from `/proc/meminfo` with the observability stack already running and
        take a little under it. On this host that was 11797M against 12309
        MiB available, out of 15673 MiB total.
      '';
    };

    tokenFile = mkOption {
      type = types.path;
      example = "/var/lib/secrets/jorb-provider.env";
      description = ''
        An environment file holding the provider's own Amp credential, read by
        systemd rather than by jorb. Run exactly one provider per token.
      '';
    };

    kernel = mkOption {
      type = types.path;
      description = "The guest kernel, from `nix build .#guest-kernel`.";
    };

    package = mkOption {
      type = types.package;
      description = "The jorb binary, which runs one job per invocation.";
    };

    prepareImagePackage = mkOption {
      type = types.package;
      description = "The script that builds the base image before the provider starts.";
    };

    ampPackage = mkOption {
      type = types.package;
      default = pkgs.amp-cli;
      defaultText = lib.literalExpression "pkgs.amp-cli";
      description = ''
        The Amp CLI that provides `orb provider`. `pkgs.amp-cli` is unfree, so
        the host's configuration has to allow it.
      '';
    };

    cloudHypervisorPackage = mkOption {
      type = types.package;
      default = pkgs.cloud-hypervisor;
      defaultText = lib.literalExpression "pkgs.cloud-hypervisor";
      description = "The VMM each orb runs in.";
    };

    stateDirectory = mkOption {
      type = types.str;
      default = "/var/lib/jorb";
      description = ''
        Where the base image and the orbs' disks live. Has to be XFS: every
        orb's disk is a reflink clone of the base image.
      '';
    };

    runtimeDirectory = mkOption {
      type = types.str;
      default = "/run/jorb";
      description = "Where the config disks and API sockets live.";
    };

    stateImage = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/var/lib/jorb.img";
      description = ''
        An XFS image to create and mount on `stateDirectory`, for a host whose
        filesystem cannot reflink. A oneshot creates it if it is missing and a
        loop mount puts it on `stateDirectory` before the provider starts;
        leave it null on a host where `stateDirectory` is already XFS or btrfs.

        It has to sit outside `stateDirectory`, or the mount would hide it.
      '';
    };

    stateImageGiB = mkOption {
      type = types.ints.positive;
      default = 128;
      description = ''
        How big that image claims to be. It is sparse, so this is what the
        filesystem believes it has rather than what it costs.
      '';
    };

    observability = {
      enable = mkEnableOption ''
        jorb's own Alloy configuration and Grafana dashboard.

        Both are installed as their own files: the owner's existing Alloy
        configuration and dashboards are never edited
      '';

      alloyConfigDirectory = mkOption {
        type = types.str;
        default = "/etc/alloy";
        description = ''
          The directory Alloy reads its configuration from. jorb adds one file
          to it, because Alloy loads every `*.alloy` file in there.
        '';
      };

      metricsEndpoint = mkOption {
        type = types.str;
        default = "http://localhost:9090/api/v1/write";
        description = "Where jorb's own Alloy sends the orbs' cgroup metrics.";
      };

      dashboardDirectory = mkOption {
        type = types.str;
        default = "/var/lib/grafana/dashboards/jorb";
        description = ''
          Where the provisioned dashboard is written. Grafana is told to read
          this directory and nothing else of the owner's is touched.
        '';
      };
    };
  };

  config = lib.mkMerge [
    (mkIf (cfg.enable && cfg.observability.enable) {
      # One more file in Alloy's directory, with the endpoint substituted.
      environment.etc."${lib.removePrefix "/etc/" cfg.observability.alloyConfigDirectory}/jorb.alloy" =
        mkIf (lib.hasPrefix "/etc/" cfg.observability.alloyConfigDirectory)
          {
            source = pkgs.replaceVars ./jorb.alloy {
              JORB_METRICS_ENDPOINT = cfg.observability.metricsEndpoint;
            };
          };

      systemd.tmpfiles.rules = [
        "d ${cfg.observability.dashboardDirectory} 0755 grafana grafana -"
        "L+ ${cfg.observability.dashboardDirectory}/jorb.json - - - - ${./jorb-dashboard.json}"
      ];

      services.grafana.provision.dashboards.settings.providers = [
        {
          name = "jorb";
          options.path = cfg.observability.dashboardDirectory;
          options.foldersFromFilesStructure = false;
        }
      ];
    })

    (mkIf cfg.enable {
      assertions = [
        {
          assertion = cfg.stateImage == null || !lib.hasPrefix "${cfg.stateDirectory}/" cfg.stateImage;
          message =
            "services.jorb.stateImage (${toString cfg.stateImage}) is inside"
            + " stateDirectory (${cfg.stateDirectory}), where mounting it would hide it.";
        }
        {
          assertion = cfg.slots <= 256;
          message = "services.jorb.slots is ${toString cfg.slots}; a slot is a byte of 10.77.S.0/30, so 256 is the most there can be.";
        }
      ];

      users.users.jorb = {
        isSystemUser = true;
        group = "jorb";
        description = "Amp orb provider";
      };
      users.groups.jorb = { };

      # One rule, one program, four variables. The provider cannot choose what
      # runs as root, only ask for a job.
      security.sudo = {
        extraRules = [
          {
            users = [ "jorb" ];
            runAs = "root";
            # No SETENV: with it, variables on the sudo command line bypass
            # env_keep, and the caller could pick LD_PRELOAD or the VMM.
            commands = [
              {
                command = "${rootSide}/bin/jorb-root";
                options = [ "NOPASSWD" ];
              }
            ];
          }
        ];
        extraConfig = ''
          Defaults:jorb env_keep += "${lib.concatStringsSep " " preserved}"
        '';
      };

      # Explicitly in system.slice, so the MemoryHigh on jorb.slice bounds the
      # orbs and not the provider that starts them. Recover cannot reach this
      # unit either way: its name is outside jorb-orb-*.
      systemd.services.jorb-provider = {
        description = "Amp orb provider";
        wantedBy = [ "multi-user.target" ];
        after = [
          "network-online.target"
          "jorb-base-image.service"
        ];
        wants = [ "network-online.target" ];
        # A provider on a host that is not ready would take jobs it cannot
        # finish, so it waits for the base image and the state directory's
        # mount.
        requires = [ "jorb-base-image.service" ];
        unitConfig.RequiresMountsFor = [ cfg.stateDirectory ];

        serviceConfig = {
          ExecStart = "${cfg.ampPackage}/bin/amp orb provider ${dispatch}/bin/jorb-dispatch";
          EnvironmentFile = cfg.tokenFile;
          User = "jorb";
          Group = "jorb";
          Slice = "system.slice";
          Restart = "always";
          RestartSec = 5;
        };

      };

      systemd.slices.jorb = {
        description = "Amp orbs";
        sliceConfig = {
          MemoryHigh = cfg.memoryHigh;
          MemoryAccounting = true;
          CPUAccounting = true;
          IOAccounting = true;
        };
      };

      # Mode 0700: a config disk under the run directory holds a token.
      systemd.tmpfiles.rules = [
        "d ${cfg.stateDirectory} 0700 root root -"
        "d ${cfg.stateDirectory}/disks 0700 root root -"
        "d ${cfg.runtimeDirectory} 0700 root root -"
      ];

      # The image pull takes minutes and needs the network, so it gets its own
      # unit and a generous timeout, and retries until it works. The
      # provider's start waits for it.
      systemd.services.jorb-base-image = {
        description = "Build the base image every orb's disk is cloned from";
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];
        unitConfig.RequiresMountsFor = [ cfg.stateDirectory ];
        environment.JORB_STATE_DIR = cfg.stateDirectory;

        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = "${cfg.prepareImagePackage}/bin/jorb-prepare-image";
          TimeoutStartSec = "30min";
          Restart = "on-failure";
          RestartSec = 30;
        };
        # Retry until the network comes back rather than giving up after five.
        startLimitIntervalSec = 0;
      };
    })

    (mkIf (cfg.enable && cfg.stateImage != null) {
      # Created once, under a temporary name and renamed, so a kill leaves
      # either no image or a formatted one.
      systemd.services.jorb-state-image = {
        description = "Create the XFS image jorb's state directory is mounted from";
        unitConfig = {
          DefaultDependencies = false;
          RequiresMountsFor = [ (dirOf cfg.stateImage) ];
        };
        path = [
          pkgs.coreutils
          pkgs.xfsprogs
        ];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
        };
        script = ''
          image=${lib.escapeShellArg cfg.stateImage}
          if [ -e "$image" ]; then
            exit 0
          fi
          partial="$image.partial"
          rm -f "$partial"
          truncate -s ${toString cfg.stateImageGiB}G "$partial"
          mkfs.xfs -q -L jorb "$partial"
          sync "$partial"
          mv "$partial" "$image"
          sync "$(dirname "$image")"
        '';
      };

      # Before tmpfiles, so the directories it creates land on the mount
      # rather than under it.
      fileSystems.${cfg.stateDirectory} = {
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

    (mkIf cfg.enable {
      # An orb that is throttled by MemoryHigh reclaims into zram rather than
      # onto a disk, which is both faster and kinder to the SSD.
      zramSwap.enable = true;

      boot.kernel.sysctl."net.ipv4.ip_forward" = 1;

      networking.nftables.enable = true;

      # Orbs are internet-only. Everything the policy forbids is in one table:
      # nothing from an orb reaches the host, nothing reaches any address that
      # is not the public internet, and IPv6 from an orb goes nowhere at all.
      networking.nftables.tables.jorb = {
        family = "inet";
        content = ''
          set not-the-internet {
            type ipv4_addr
            flags interval
            comment "every range an orb has no business reaching"
            elements = {
              0.0.0.0/8,          # this host, this network
              10.0.0.0/8,         # private, including the slots themselves
              100.64.0.0/10,      # carrier NAT
              127.0.0.0/8,        # loopback
              169.254.0.0/16,     # link-local, which is where cloud metadata lives
              172.16.0.0/12,      # private
              192.0.0.0/24,       # IETF protocol assignments
              192.0.2.0/24,       # documentation
              192.168.0.0/16,     # private
              198.18.0.0/15,      # benchmarking
              198.51.100.0/24,    # documentation
              203.0.113.0/24,     # documentation
              224.0.0.0/4,        # multicast
              240.0.0.0/4,        # reserved, including the broadcast address
            }
          }

          chain input {
            type filter hook input priority filter; policy accept;

            # Nothing an orb sends is for the host, whatever address it is
            # addressed to. This is what makes the tap's own address, and every
            # other address the host holds, unreachable from a guest.
            iifname "jorb*" drop
          }

          chain forward {
            type filter hook forward priority filter; policy accept;

            # A slot is an IPv4 /30 and nothing else, so IPv6 from an orb is
            # not a route this host offers.
            iifname "jorb*" meta nfproto ipv6 drop

            iifname "jorb*" ip daddr @not-the-internet drop
          }

          chain postrouting {
            type nat hook postrouting priority srcnat; policy accept;

            # The slots' own addresses, leaving by any interface but a tap.
            ip saddr 10.77.0.0/16 oifname != "jorb*" masquerade
          }
        '';
      };
    })
  ];
}
