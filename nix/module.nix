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

  # The root side: the only program the sudo rule allows, and the only place
  # JORB_* comes from.
  rootSide = pkgs.writeShellApplication {
    name = "jorb-root";
    runtimeInputs = [
      config.systemd.package
      cfg.cloudHypervisorPackage
    ];
    text = ''
      # Set, not defaulted: whatever the caller had to say about the host is
      # not interesting.
      export JORB_SLOTS=${toString cfg.slots}
      export JORB_VCPUS=${toString cfg.vcpus}
      export JORB_MEMORY_MIB=${toString cfg.memoryMiB}
      export JORB_KERNEL=${cfg.kernel}
      export JORB_STATE_DIR=${cfg.stateDirectory}
      export JORB_RUN_DIR=${cfg.runtimeDirectory}

      exec ${cfg.package}/bin/jorb
    '';
  };

  # What `amp orb provider` runs, as the unprivileged provider user.
  dispatch = pkgs.writeShellApplication {
    name = "jorb-dispatch";
    runtimeInputs = [ pkgs.sudo ];
    text = ''
      # -n so a missing sudo rule fails rather than waits for a password that
      # no one is there to type.
      exec sudo -n ${rootSide}/bin/jorb-root
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
        taking the host down. `tests/host/g1-free-memory.sh` prints the
        absolute figure for this host.
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
            commands = [
              {
                command = "${rootSide}/bin/jorb-root";
                options = [
                  "NOPASSWD"
                  "SETENV"
                ];
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
        after = [ "network-online.target" ];
        wants = [ "network-online.target" ];

        serviceConfig = {
          # + runs it as root: building the base image needs to unpack an image
          # with its ownership and setuid bits intact.
          ExecStartPre = "+${cfg.prepareImagePackage}/bin/jorb-prepare-image";
          ExecStart = "${cfg.ampPackage}/bin/amp orb provider ${dispatch}/bin/jorb-dispatch";
          EnvironmentFile = cfg.tokenFile;
          User = "jorb";
          Group = "jorb";
          Slice = "system.slice";
          Restart = "always";
          RestartSec = 5;
        };

        environment.JORB_STATE_DIR = cfg.stateDirectory;
        path = [ pkgs.sudo ];
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

      systemd.tmpfiles.rules = [
        "d ${cfg.stateDirectory} 0700 root root -"
        "d ${cfg.stateDirectory}/disks 0700 root root -"
        "d ${cfg.runtimeDirectory} 0700 root root -"
      ];

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
