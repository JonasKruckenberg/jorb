# jorb

Self-hosted Amp orb provider. `amp orb provider` runs `jorb` once per job; `jorb` turns the job into a Cloud Hypervisor microVM on one host: a Mac mini M2, 16 GiB RAM, aarch64 Asahi NixOS, 16K host pages.

Read `PLAN.md` (what is left) and `PROGRESS.md` (state, gate results, open questions) before any work.

## Priorities, in order

1. Correctness by construction: make invalid states unrepresentable or unreachable.
2. Simplicity: the fewest states, branches and moving parts that satisfy the contract.
3. Small, typed interfaces. No speculative code.
4. Observability: every job explains itself in structured logs.

The earlier wins a conflict. A feature not in `PLAN.md` is out of scope; argue for it under "Proposals" in `PROGRESS.md`.

## Contract with Amp

Source of truth: https://ampcode.com/docs/orbs/self-hosted. Re-read it rather than trust memory.

- Input is environment only: `ORB_OPERATION` (`spawn` | `pause` | `delete`), `ORB_ID`, and on spawn `ORB_TOKEN` and optionally `AMP_URL`. jorb takes no arguments; one is an error. v1 ignores `ORB_IMAGE`, `ORB_SNAPSHOT_ID` and `ORB_TYPE`, and the sudo rule does not carry them.
- Every operation is safe to repeat and finishes within five minutes.
- Exit 0 if and only if the operation's postcondition was observed.
- Assume at-least-once delivery and concurrent jobs.

## Model

An orb is two facts: *disk exists* and *VM running*. "Running without disk" is unreachable: create the disk before starting, stop before unlinking. Every job takes one global lock (`File::lock` on `/var/lib/jorb/lock`), runs `recover`, then its operation:

| Operation | Composition |
|---|---|
| pause | `stop` |
| delete | `stop`, `remove_disk` |
| spawn | nothing if running with a byte-identical config disk; otherwise `stop`, `ensure_disk`, `start` |

Each primitive establishes its own preconditions (truncate `disks/.tmp`, remove a stale socket, overwrite the config disk) and observes its postcondition: systemd's answer, the VMM's, a file. Never another tool's exit code.

## Failure model

jorb is crash-only: the start path is the recovery path.

- Any previous job may have died at any instruction; `kill -9`, a power cut and an error all leave something the next job handles the same way.
- `recover` stops every `jorb-orb-*` unit whose VM it cannot confirm running, whosever it is. Under the lock no job is mid-start, so such a unit is debris or a VMM that stopped answering, and it holds a slot.
- No cleanup on the way out: none in error branches, no `Drop` for external state, no signal handlers. A kill skips them anyway.
- Fail early and loudly: one error event with the evidence, non-zero exit. No retries, fallbacks or silent repair. An observed invariant violation is a bug to report, not to paper over. Errors, not `panic!`.
- A healthy orb is left alone. Orb units have no `Restart=`; only the provider restarts itself.
- `jorb-init` powers off on any setup failure and when bootstrap exits, so a running orb means bootstrap is alive. Pause is a power cut; ext4 replays its journal on the next boot.

## Fixed interfaces

Changing anything here is a design change. Propose it first.

**`ORB_ID`** matches `[A-Za-z0-9][A-Za-z0-9_-]{0,63}`. It is used unescaped as a file name, unit name and hostname.

**Host paths.** The Nix module creates the directories (0700) and the base image.

| Path | Purpose |
|---|---|
| `/var/lib/jorb/base.raw` | ext4 base image, 64 GiB sparse; holds `/jorb-init` and `/etc/jorb-env` |
| `/var/lib/jorb/disks/<id>.raw` | reflink clone of the base image (XFS), via `disks/.tmp`, fsync, rename |
| `/var/lib/jorb/lock` | the global lock |
| `/run/jorb/<id>.cfg` | config disk, mode 0600, present only while the orb runs |
| `/run/jorb/<id>.sock` | Cloud Hypervisor API socket |

**Units** are `jorb-orb-<id>.service` in `jorb.slice`, started with `systemd-run --collect`. The prefix is what keeps every other unit, the provider's included, out of `recover`'s reach.

**Slots.** `S` in `0..JORB_SLOTS`, at most 256: tap `jorb<S>`, host `10.77.S.1/30`, guest `10.77.S.2`. A slot is free when its tap is absent; no free slot is the capacity error.

**Config disk** (`jorb-config-disk`): UTF-8 `KEY=VALUE\n` lines, NUL-padded to 4096 bytes, second disk, read-only. Keys `ORB_TOKEN`, `AMP_URL`; values with a newline or NUL are refused. "Identical" is byte equality.

**VM shape**, all on Cloud Hypervisor's command line so the VMM boots the VM as it starts and exits when the guest powers off:

- Kernel `$JORB_KERNEL`, no initramfs; cmdline `console=ttyAMA0 root=/dev/vda rw init=/jorb-init ip=10.77.S.2::10.77.S.1:255.255.255.252:<orb-id>:eth0:off:1.1.1.1:9.9.9.9`.
- Disks: root (read-write), then config disk (read-only), both `image_type=raw`.
- The slot's tap; `$JORB_VCPUS` vCPUs; `$JORB_MEMORY_MIB` memory, THP off; balloon of size 0 with free page reporting; serial to stdout (the journal), console off.
- Running means `ch-remote info` reports `Running`.

**`jorb-init`** (guest PID 1), in order:

1. Mount `proc`, `sysfs`, `devpts` (without `nodev`), tmpfs on `/dev/shm` (1 GiB) and `/run`; link `/dev/fd` and the standard streams into `/proc/self/fd`.
2. Write `100` to `vm.dirty_expire_centisecs` and `vm.dirty_writeback_centisecs`.
3. Copy `/proc/net/pnp` to `/etc/resolv.conf`; write `/etc/hosts` with `127.0.0.2 <hostname>`.
4. Environment: `/etc/jorb-env`, then `HOME=/home/user`, `USER=user`, then the config disk. No `ORB_TOKEN` is a setup failure.
5. Spawn `setpriv --reuid=user --regid=user --init-groups -- /usr/local/bin/amp orb bootstrap` in `/home/user`.
6. Reap until bootstrap exits; then sync, log its status in one info event, power off.

jorb knows nothing about the guest and jorb-init nothing about Amp; the config disk is the whole interface.

**Network policy.** Orbs are internet-only: nothing they send reaches the host or a private, link-local, CGNAT, loopback or multicast range; IPv6 is dropped; a tap may send only from its slot's guest address; nothing reaches a tap that the orb did not ask for. The policy is the `inet jorb` table in the Nix module. It only drops, so `start` refuses to run without it.

## Crates

| Crate | Role | Dependencies (allowlist) |
|---|---|---|
| `jorb` (bin) | One job: parse, lock, recover, operate, report | `jorb-config-disk`, `tokio` (current-thread), `serde_json`, `tracing`, `tracing-subscriber` (json), `anyhow`, `rustix`, `secrecy` |
| `jorb-init` (bin, static musl) | Guest PID 1, synchronous | `jorb-config-disk`, `rustix`, `tracing`, `tracing-subscriber` (json), `anyhow` |
| `jorb-config-disk` (lib) | The config disk's format, shared so the two cannot drift | none |

## Rust conventions

- Edition 2024; toolchain from the flake. Add dependencies with `cargo add`, then read docs.rs for that version rather than trust memory. `unsafe_code = "forbid"`; `rustix` covers the syscalls.
- Each module opens with a short `//!` paragraph: what it is for and what it refuses to know. Code reads top-down, entry point first. Comment only what the code cannot say.
- Use the project's words (orb, job, operation, primitive, slot, base image, disk, config disk, gate) and whole words otherwise (`id`, `vm`, `ip`, `api`, `cpu`, `mib` allowed). No `Manager`, `Handler`, `Helper`, `Util`, `Service`, `Context`, `Info`, `Data`. A new type or module must remove a state or a duplication.
- Parse at the edge, once: `OrbId`, `Job`, `Settings`, `ConfigDisk` are built only by fallible parsers. No boolean parameters in public functions. No traits or generics for hypothetical use. No handle type for what a path already names.
- Errors: `anyhow` with `.context(...)`; no `unwrap`, `expect`, `panic!`, `todo!`, `dbg!` outside tests (clippy enforces it). Never build a shell string.
- Bounds: every loop's exit is visible at the loop (a finite iterator or a deadline); every wait and external call has a timeout; the job has a deadline below Amp's. Durations and counts are named in `jorb/src/limits.rs`; names, paths and programs live where they are used. The one unbounded loop is `jorb-init`'s reap, in a function returning `!`. No recursion.
- Logging is `tracing`, JSON. Dashboard fields: `op`, `orb`, `slot`, `outcome`, `elapsed_ms`. `ORB_TOKEN` is a `SecretString` from parsing on and never reaches logs, argv, errors, subprocesses or a persistent disk.
- Unit-test pure functions; tests need no root, KVM or network. Host checks are `tests/host/`.

## Security

The provider is a closed-source, self-updating CLI running unprivileged, and reaches `jorb` through one sudo rule: root only, no `SETENV`, preserving only the four `ORB_*`/`AMP_URL` variables. The root-side wrapper unsets everything else and sets `PATH` and every `JORB_*` itself. Input validation in `jorb` is the privilege boundary.

## Commands

```sh
nix develop
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
nix flake check                     # the above plus module evaluation, nft --check
sudo -E tests/host/matrix.sh        # on the host, with the module installed
```

## Working method

- Work from `PLAN.md`. After each piece: run its checks, commit (what, and why), update `PROGRESS.md` with results and open questions. Git plus `PROGRESS.md` is the memory.
- Report faithfully: a failed or unrun check is recorded as such. Never weaken a check to pass it.
- `[host]` checks need the machine. Work out whether one needs root or only `/dev/kvm` before calling it blocked; otherwise write the script, record "awaiting host run", and continue. Do not infer a result.
- At the end of a milestone, have a reviewer with fresh context check the diff against this file.

## Ask first

- Touching host partitions, secrets, or NixOS configuration outside the jorb module.
- Changing a fixed interface, adding a dependency outside the allowlist, or relaxing the network policy.
- Recursion, or a second loop without a visible bound.
- Acting on a gate result that contradicts an assumption in `PLAN.md`.
