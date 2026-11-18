# jorb v1 plan

Read `AGENTS.md` first. Tasks run in order unless "Depends" allows otherwise. `[host]` marks checks that need the Mac mini (aarch64, KVM, root); `[human]` marks steps only the owner can do.

**v1 sizing:** 1 slot, 4 vCPUs, 6 GiB nominal guest memory. All three, plus `MemoryHigh`, are module options.

## Assumptions to verify

Each is resolved by a gate in T3. Until then, code that depends on one carries `// ASSUMES A<n>`. A gate that fails is an "ask first" event, not something to work around.

| ID | Assumption | Gate |
|---|---|---|
| A1 | Cloud Hypervisor from nixpkgs boots a 4K-page guest on the 16K-page host | G4 |
| A2 | Disks appear as `/dev/vda` (root) and `/dev/vdb` (config) in attach order | G4 |
| A3 | Serial is `ttyAMA0` and its output reaches the journal | G4 |
| A4 | The guest clock is correct at boot (PL031 RTC) | G4 |
| A5 | Free page reporting returns freed guest memory to the host | G5 |
| A6 | The host's PMD huge page size is 32 MiB (the reason THP is off) | G2 |
| A7 | The Amp arm64 image has glibc, `setpriv`, user `user`, `/usr/local/bin/amp` | G6 |
| A8 | `amp orb bootstrap` stays in the foreground | G6 |
| A9 | progenitor accepts Cloud Hypervisor's OpenAPI spec | T2 |
| A10 | A progenitor client works over `reqwest`'s unix-socket transport | T2 |
| A11 | Alloy's cAdvisor exporter exposes `/jorb.slice/*` cgroups; the journal reaches Loki | G7 |
| A12 | An `amp` CLI with `orb provider` is installable on NixOS aarch64 | G8 |

## Milestone 1: foundations

### T0. Repository skeleton
- **Deliver:** `flake.nix` (dev shell with Rust toolchain, `cloud-hypervisor`, and `CH_OPENAPI_SPEC` pointing into that package's source); Cargo workspace with `jorb-vmm`, `jorb`, `jorb-init`; `[workspace.lints]` (`unsafe_code = "forbid"`; clippy `all`, `unwrap_used`, `expect_used`, `panic`, `todo`, `dbg_macro`, `print_stdout`, `print_stderr` denied, with unwrap allowed in tests); a `constants.rs` at the root of `jorb` and `jorb-init`; `PROGRESS.md` with sections Status, Gate results, Open questions, Proposals. If the toolchain's clippy has an `infinite_loop` lint, deny it too (check; do not assume).
- **Accept:** `nix flake check` passes with empty crates.

### T1. Guest kernel
- **Deliver:** `nix/kernel.nix` producing an uncompressed arm64 `Image`. 4K pages, no modules, no initramfs. Start from Cloud Hypervisor's reference aarch64 guest config if its source tree has one (look; do not assume), otherwise `defconfig` plus a fragment.
- **Required built-ins:** `ARM64_4K_PAGES`, `PCI_HOST_GENERIC`, `VIRTIO_PCI`, `VIRTIO_BLK`, `VIRTIO_NET`, `VIRTIO_BALLOON`, `PAGE_REPORTING`, `HW_RANDOM_VIRTIO`, `EXT4_FS`, `TMPFS`, `DEVTMPFS`, `DEVTMPFS_MOUNT`, `UNIX98_PTYS`, `IP_PNP`, `SERIAL_AMBA_PL011` with console, `RTC_DRV_PL031`, `RTC_HCTOSYS`.
- **Accept:** `nix build .#guest-kernel` succeeds `[host]` (needs an aarch64 builder); a check script greps the final `.config` and fails on any missing option.

### T2. Generated client spike (resolves A9, A10)
- **Deliver:** `jorb-vmm/build.rs` generating a builder-style client from `$CH_OPENAPI_SPEC` into `OUT_DIR`; a private `generated` module; an example binary that calls `vmm.ping`.
- **If progenitor rejects the spec:** patch the spec inside `build.rs` with the smallest transformation that works, comment each patch with the reason, and list them in `PROGRESS.md`. Do not hand-write the client or switch generators without asking.
- **Accept:** `cargo build -p jorb-vmm` passes; `[host]` the example prints the version from a live `cloud-hypervisor --api-socket`.

### T3. Host gates `[host]`
- **Deliver:** one script per gate in `tests/host/`, each printing a single PASS or FAIL line plus evidence; a Nix-built busybox ext4 test image for G4 and G5. Record every result in `PROGRESS.md`.
- **Depends:** T1, T2.

A gate answers one question about this host or about Cloud Hypervisor, and the
answer goes in `PROGRESS.md`. A gate whose answer is settled and will not
change is then deleted: the knowledge lives in `PROGRESS.md`, and the script
is scaffolding. G1 (free memory), G2 (`hpage_pmd_size`) and G8 (how `amp`
installs) went that way on 2026-11-18.

| Gate | Needs | Check |
|---|---|---|
| boot-check | kvm | The config jorb itself sends boots: guest up, `vda`/`vdb` order, clock right. Covers A1, A2, A4 and runs on every change |
| G3 | — | Replaced by `jorb preflight`, which probes the clone itself and tells a filesystem that cannot do it from a directory it may not write to |
| G4 | root | The final shape through `systemd-run`: tap up, the guest takes its `ip=` address, console lines in `journalctl -u`. What is left of A3 |
| G5 | root | In the guest, allocate then free 1 GiB; the unit's cgroup `memory.current` falls by most of it within a minute |
| G6 | network | Pull the arm64 image, pin its digest, unpack; verify A7; run bootstrap with a dummy token and observe whether it forks away. Rerun whenever the pinned digest is bumped |
| G7 | root | Start any unit in `jorb.slice`; its cgroup metrics appear in Prometheus and its journal lines in Loki |

## Milestone 2: components

### T4. `jorb-vmm` API
- **Deliver:** `VmBuilder` (kernel, cmdline, cpus, memory, `without_huge_pages`, `disk`, `read_only_disk`, `tap`, `free_page_reporting`, serial to stdout) with `async fn boot(self, socket) -> Result<()>` performing create then boot; `state(socket) -> Result<VmState>` answering `Absent` for a socket that is gone, dead or has no VM; a `thiserror` error enum; an `API_REQUEST_TIMEOUT` at the crate root applied to every request. An API socket is the only name for a VM; there is no handle type. Nothing from the generated module is re-exported.
- **Accept:** golden test comparing the builder's JSON for the fixed VM shape against a checked-in file; a `//!` paragraph on the crate root stating what the crate refuses to know; `[host]` example boots the G4 image.

Illustrative shape, not a specification:

```rust
VmBuilder::new(kernel, cmdline)
    .cpus(4).memory_mib(6144).without_huge_pages()
    .disk(root_disk).read_only_disk(config_disk)
    .tap(slot.tap_name(), slot.host_address(), GUEST_NETMASK)
    .free_page_reporting()
    .boot(&socket).await?;
let running = jorb_vmm::state(&socket).await? == VmState::Running;
```

### T5. `jorb-init`
- **Deliver:** the seven steps in `AGENTS.md`, as small functions; config-disk parsing shared in spirit with `jorb` but not via a shared crate (two small parsers are simpler than a third crate; keep their tests identical).
- **Accept:** unit tests for env-file and config-disk parsing (padding, missing token, empty lines); `nix build .#jorb-init` yields a static aarch64 musl binary (`file` shows statically linked); `[host]` boots under G4's harness with the busybox image and logs JSON to the journal.

### T6. Base image script
- **Deliver:** `nix/image.nix` exposing a `jorb-prepare-image` script: `skopeo copy` the pinned digest; unpack as root with `umoci`; install `/jorb-init`; write `/etc/jorb-env` from the image config's `Env`; write `/etc/hosts`; `mkfs.ext4 -d` into a 64 GiB sparse file; rename into place atomically. Idempotent: exits early if the base exists.
- **Accept:** shellcheck clean; `[host]` produces `/var/lib/jorb/base.raw`, and `debugfs` shows `/jorb-init`, `/etc/jorb-env`, and setuid bit intact on `sudo`.

### T7. `jorb` types and pure functions
- **Deliver:** `OrbId`, `Operation`, `Request` (from `clap` with `env`), `Settings` (`JORB_*`), `Slot` with address functions, `ConfigDisk`, `cmdline(slot, id)`.
- **Accept:** unit tests including rejections: empty id, 65 characters, leading `-`, `../x`, a dot, a slash, newline or NUL in a token; address math for slots 0 and 255; cmdline golden string.

### T8. `jorb` primitives and operations
- **Deliver:**
  - `recover`: list `jorb-orb-*` units (bounded by a named limit); stop each whose VM is not `Running` via `state(socket)`. Runs first in every job.
  - `stop`: `systemctl stop`; postcondition unit inactive; remove the socket.
  - `ensure_disk`: if absent, reflink base to `disks/.tmp`, fsync file, rename, fsync directory.
  - `remove_disk`: unlink if present; remove the config file.
  - `start`: remove any stale socket; write the config disk; pick the lowest free slot; `systemd-run --collect --unit jorb-orb-<id> --slice jorb.slice -p Type=exec -p TimeoutStopSec=5 -p OOMScoreAdjust=500` running Cloud Hypervisor with the API socket; wait up to `API_SOCKET_WAIT` for the socket; `VmBuilder::boot`; postcondition `state == Running`. On failure, return the error and clean nothing up; the next job's `recover` removes the debris.
  - Operations as the compositions in `AGENTS.md`; `main` takes the lock, opens the root span, runs `recover` then the operation under `JOB_DEADLINE`, maps the result to the exit code.
  - `constants.rs` holding every size, count, duration and fixed name used above (the `5` in the `systemd-run` line is `UNIT_STOP_TIMEOUT`); each subprocess runs under a timeout.
- **Logging:** root span `job` with `op` and `orb`; one `#[instrument]` span per primitive (skip secrets); a final event with `outcome`, `state_before`, `state_after`.
- **Accept:** clippy and tests pass; a test proves the token never appears in `Debug` or log output of `Request`; behaviour is verified in T11.

## Milestone 3: host integration

### T9. NixOS module
- **Deliver:** `nix/module.nix` with `services.jorb.{enable, slots, vcpus, memoryMiB, memoryHigh, tokenFile}`:
  - user `jorb`; `jorb-provider.service` running `amp orb provider <wrapper>` with `EnvironmentFile = tokenFile`, `Restart = always`;
  - the wrapper (`sudo -n` with the four preserved variables) and the root-side wrapper that sets `JORB_*`; the matching sudo rule;
  - `jorb.slice` with `MemoryHigh`; tmpfiles for `/var/lib/jorb/disks` and `/run/jorb`;
  - `zramSwap.enable = true`; `net.ipv4.ip_forward = 1`;
  - one nftables table implementing the network policy, with masquerade for `jorb*`;
  - `jorb-prepare-image` as `ExecStartPre` of the provider.
- **Accept:** `nix flake check` evaluates a test configuration that enables the module; the generated ruleset passes `nft -c -f`. `[human]` wires the module and token file into the real host config.

### T10. Observability
- **Deliver:** `nix/jorb.alloy`, installed by a module option and not by editing the owner's existing Alloy config: cAdvisor scoped to `/jorb.slice`, relabelling the cgroup path to an `orb` label. A provisioned Grafana dashboard: per-orb CPU, memory and IO; tap bytes per slot; zram original versus compressed size; job durations and failures from the JSON logs; "no free slot" count.
- **Accept:** `[host]` every panel returns data during T11.

## Milestone 4: verification `[host]`

### T11. Behaviour matrix
- **Deliver:** `tests/host/matrix.sh`, using the busybox image and a dummy token:
  1. For each start state (absent, stopped, running) and each operation, run the operation twice; assert exit 0, the target state, and no stray units, taps, sockets or `.tmp` files.
  2. Kill loop: 50 times, run a random operation under `timeout -s KILL` with a random delay up to 1.5 s; then run spawn, pause, delete and assert convergence. No test hooks in production code.
  3. Spawn with an identical config on a running orb leaves the VM's boot time unchanged; a changed token restarts it.
  4. Network: from the guest, the host's tap address, the host's LAN address, a LAN neighbour and `169.254.169.254` are unreachable; `https://ampcode.com` is reachable.
  5. Memory: a build-like workload followed by idle shows the cgroup footprint falling, and zram absorbing pages once `MemoryHigh` is reached.
  6. Bounds: with `JORB_CH_BIN` pointed at a program that never opens the socket, spawn fails within `API_SOCKET_WAIT` and exits non-zero; the next job of any orb removes the leftover unit.
  7. Recovery across orbs, with one slot: kill a spawn of orb A after its tap appears but before boot completes; a spawn of orb B must then succeed. Then cut power to the test VM's host process (`kill -9` on Cloud Hypervisor) and confirm the next spawn boots the same disk.
- **Accept:** all seven sections PASS; results and any flake recorded in `PROGRESS.md`.

### T12. End to end `[human]`
- Create a thread on the provider; confirm tools connect; archive (pause); send a message (spawn, disk intact); delete the thread (disk gone). Capture the dashboard during the run.

## Definition of done for v1

- T0 through T12 complete, with evidence in `PROGRESS.md`.
- Every assumption A1 to A12 is marked verified or has an agreed resolution; no `// ASSUMES` comment remains for a verified one.
- `nix flake check` passes from a clean checkout.

## Deferred: do not build

In likely order: guest memory line on the console; a second slot; snapshots (`ORB_SNAPSHOT_ID`); graceful stop (power button plus an init handler; it adds a second shutdown path, so weigh it against the failure model before building); `ORB_TYPE` sizes; RAM snapshots; ZFS. The places to extend already exist (`ConfigDisk` keys, new methods in `jorb-vmm`, a second clone source in `ensure_disk`); do not build ahead of them.
