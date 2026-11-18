# jorb v1 plan

Read `AGENTS.md` first. T0–T11 are built; git history has their plans. `[host]` needs the Mac mini; `[owner]` needs a decision from the owner.

v1 sizing: 1 slot, 4 vCPUs, 6 GiB per orb, `MemoryHigh` 75%. All are module options.

## Assumptions

Each was a gate. A gate that fails is an "ask first" event, not something to work around. State is in `PROGRESS.md`.

| ID | Assumption |
|---|---|
| A1 | Cloud Hypervisor boots a 4K-page guest on the 16K-page host |
| A2 | Disks appear as `/dev/vda` then `/dev/vdb`, in attach order |
| A3 | Serial is `ttyAMA0` and reaches the journal |
| A4 | The guest clock is right at boot (PL031) |
| A5 | Free page reporting returns freed guest memory to the host |
| A6 | The host's PMD huge page is 32 MiB, which is why THP is off |
| A7 | The Amp arm64 image has glibc, `setpriv`, user `user`, `/usr/local/bin/amp` |
| A8 | `amp orb bootstrap` stays in the foreground |
| A11 | Alloy's cAdvisor exporter sees `/jorb.slice`; the journal reaches Loki |
| A12 | `amp-cli` with `orb provider` installs on NixOS aarch64 |
| A13 | Cloud Hypervisor's command line takes the whole VM shape, and the VMM exits when the guest powers off |

## Remaining

1. **`[host]` Verify the command-line boot (A13).** Run every matrix section. The boot path changed from the API to the command line after the last host run, so nothing that boots has been observed since.
2. **`[host]` The review's checks.** `sudo -u jorb sudo -n FOO=1 <jorb-root>` is refused; `sudo -u jorb env ORB_OPERATION=pause ORB_ID=x <jorb-dispatch>` exits 0; a boot with no base image ends with the provider active.
3. **`[host]` G7** (`tests/host/observability.sh`): the orbs' metrics reach Prometheus and nothing else from cAdvisor does; the journal reaches Loki. Resolves A11.
4. **`[host]` Sandbox the orb units**, one `systemd-run --property=` at a time, running matrix sections 1, 3, 5 and 7 after each, with `systemd-analyze security` before and after: `NoNewPrivileges`; `PrivateTmp`, `ProtectHome`; `DevicePolicy=closed` with `/dev/kvm` and `/dev/net/tun`; `ProtectSystem=strict` with the disk and run directory writable; `CapabilityBoundingSet=CAP_NET_ADMIN`; `ProtectKernel*`, `ProtectControlGroups`, `RestrictNamespaces`.
5. **`[owner]` End to end:** a real Amp thread: create, archive (pause), resume with the disk intact (spawn), delete. Capture the dashboard.

## Deferred: do not build

In likely order: a second slot; snapshots (`ORB_SNAPSHOT_ID`); graceful stop (weigh it against the failure model first); `ORB_TYPE` sizes; RAM snapshots; ZFS; several hosts.
