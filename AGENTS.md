# jorb

Self-hosted Amp orb provider. `amp orb provider` runs the `jorb` binary once per job; `jorb` turns that job into a Cloud Hypervisor microVM on one host: a Mac mini M2, 16 GiB RAM (15673 MiB reported), aarch64 Asahi NixOS, 16K host pages.

Before any work, read `PLAN.md` (tasks) and `PROGRESS.md` (current state, gate results, open questions).

## Priorities, in order

1. Correctness by construction: make invalid states unrepresentable or unreachable.
2. Simplicity: the fewest states, branches and moving parts that satisfy the contract.
3. Composable interfaces: small, typed boundaries that can be extended later. No speculative code.
4. Observability: every job explains itself in structured logs.

When two conflict, the earlier wins. v1 is deliberately minimal. A feature that is not in `PLAN.md` is out of scope; if you think one is necessary, write the case under "Proposals" in `PROGRESS.md` and continue with the plan.

## Contract with Amp

Source of truth: https://ampcode.com/docs/orbs/self-hosted. Re-read it when in doubt rather than relying on memory.

- Input is environment variables only: `ORB_OPERATION` (`spawn` | `pause` | `delete`), `ORB_ID`, and on spawn `ORB_TOKEN` and optionally `AMP_URL`.
- Amp always calls jorb with no arguments, and jorb takes none: any argument is an error, reported as one JSON event. Making a host ready is the Nix module's job, declaratively; the `jorb` binary runs jobs and nothing else.
- v1 ignores `ORB_IMAGE`, `ORB_SNAPSHOT_ID` and `ORB_TYPE`. Log that once at debug level.
- Every operation must be safe to repeat and must finish within five minutes.
- Exit 0 if and only if the operation's postcondition was observed.
- Retry and concurrency behaviour is undocumented. Assume at-least-once delivery and concurrent jobs.

## Model

Per-orb state is two facts: *disk exists* and *VM running*. "Running without disk" must be unreachable, so: create the disk before starting, stop before unlinking.

Operations are target states, composed from four primitives:

| Operation | Composition |
|---|---|
| pause | `stop` |
| delete | `stop`, `remove_disk` |
| spawn | no-op if already running with a byte-identical config disk; otherwise `stop`, `ensure_disk`, `start` |

Every job runs `recover` first, then its operation.

Rules that keep this correct:

- Each primitive verifies its postcondition by observation (API state, file existence, interface presence), not by another tool's exit code.
- One global lock (`/var/lib/jorb/lock`, std `File::lock`) is held for the whole job, so no interleavings exist.

## Failure model

jorb is crash-only: the start path is the recovery path, and there is no other.

- Assume any previous job was killed at any instruction. `kill -9`, a host power cut and an ordinary error must all leave something the next job handles the same way.
- `recover` stops every `jorb-orb-*` unit whose VM is not confirmed `Running` by its API: any other answer, or no answer within the API timeout, means stop it. Under the lock no job is mid-start, so such a unit is debris from a job that died. This is what frees a slot held by a half-started VM, including one that belongs to a different orb.
- Each primitive establishes its own preconditions on entry rather than trusting a predecessor's exit: truncate `disks/.tmp`, remove a stale socket, overwrite the config disk.
- Write no cleanup that runs on the way out: no cleanup in error branches, no `Drop` for external state, no signal handlers. That code would be a second path that a kill skips anyway.
- Fail early and loudly. On any unexpected condition, log one error event with the evidence and exit non-zero. No retries inside a job, no fallbacks, no silent repair. "Crash" here means an error return with context, not `panic!`; the clippy ban on panics stays because an error carries a better message.
- An observed invariant violation (for example a running VM without a disk) is a bug: report it as an error, do not paper over it.
- A healthy orb is left alone. The spawn no-op on an identical config disk is part of recovery, not an exception to it.
- Orb units have no `Restart=`. A VM that dies stays stopped until the next spawn. Only the provider service restarts itself.
- `jorb-init` follows the same rule: if any setup step fails, it logs the error and powers off, so the unit stops and the journal shows why.
- Pause is already this model applied to the guest: a power cut, with ext4 journal replay on the next boot as the recovery.

## Fixed interfaces

Changing anything in this section is a design change. Propose it first.

**`ORB_ID`** must match `[A-Za-z0-9][A-Za-z0-9_-]{0,63}`. It is used unescaped as a file name, unit name and hostname, which is why dots and slashes are excluded.

**Host paths**

| Path | Purpose |
|---|---|
| `/var/lib/jorb/base.raw` | ext4 base image, 64 GiB sparse, built by the image script; contains `/jorb-init` and `/etc/jorb-env` |
| `/var/lib/jorb/disks/<id>.raw` | reflink clone of the base image (XFS). Write `disks/.tmp`, fsync, rename |
| `/var/lib/jorb/lock` | global lock |
| `/run/jorb/<id>.cfg` | config disk, mode 0600 |
| `/run/jorb/<id>.sock` | Cloud Hypervisor API socket |

Units are `jorb-orb-<id>.service` inside `jorb.slice`, started with `systemd-run --collect`. The `jorb-orb-` prefix is the whole reason recover is safe: no other unit on the host, the provider's own `jorb-provider.service` and the gates' `jorb-gate-*` included, can be read as an orb's.

**Slots.** Slot `S` ranges over `0..JORB_SLOTS`, at most 256. Tap `jorb<S>`, host address `10.77.S.1/30`, guest address `10.77.S.2`. A slot is free when `/sys/class/net/jorb<S>` is absent. No free slot is the capacity error; there is no other counter.

**Config disk.** UTF-8 `KEY=VALUE\n` lines, NUL-padded to 4096 bytes, attached read-only as the second disk (`/dev/vdb`). v1 keys: `ORB_TOKEN`, `AMP_URL`. Reject values containing a newline or NUL. "Identical" means byte equality with the existing file.

**VM shape**

- Kernel `$JORB_KERNEL`, no initramfs.
- Cmdline: `console=ttyAMA0 root=/dev/vda rw init=/jorb-init ip=10.77.S.2::10.77.S.1:255.255.255.252:<orb-id>:eth0:off:1.1.1.1:9.9.9.9`
- Disks: root (read-write), config disk (read-only), in that order. Both are raw files and both declare `image_type: Raw`; Cloud Hypervisor refuses to boot a disk whose image type it was not told.
- Net: the slot's tap with its host address and mask.
- `$JORB_VCPUS` vCPUs, `$JORB_MEMORY_MIB` memory, transparent huge pages off.
- Balloon of size 0 with free page reporting on.
- Serial to stdout (so it reaches the journal); virtio console off.

**`jorb-init` (guest PID 1), in order**

1. Mount `proc`, `sysfs`, `devpts` (without `nodev`), tmpfs on `/dev/shm` (1 GiB) and `/run`; link `/dev/fd`, `/dev/stdin`, `/dev/stdout` and `/dev/stderr` into `/proc/self/fd`. The kernel mounts devtmpfs itself.
2. Write `100` to `vm.dirty_expire_centisecs` and `vm.dirty_writeback_centisecs`. Pause is a power cut; this shrinks the loss window to about a second.
3. Copy `/proc/net/pnp` to `/etc/resolv.conf`, and write `/etc/hosts` with `127.0.0.2 <hostname>` so the orb id resolves.
4. Build the environment: `/etc/jorb-env`, then `HOME=/home/user`, `USER=user`, then the config disk's keys.
5. If `ORB_TOKEN` is missing, or any step above failed, log an error and power off. A stopped unit is the truthful state.
6. Spawn `setpriv --reuid=user --regid=user --init-groups -- /usr/local/bin/amp orb bootstrap` in `/home/user`.
7. Reap children until bootstrap exits; then sync, log its status in one info event, and power off. A running orb means bootstrap is alive.

`jorb-init` knows nothing about Amp's job protocol; `jorb` knows nothing about guest internals. The config disk is the whole interface between them.

**Network policy.** Orbs are internet-only. No guest-initiated packet may reach the host or any private, link-local, CGNAT, loopback or multicast range, and IPv6 from orbs is dropped. Policy lives in one nftables table in the Nix module, matching `iifname "jorb*"`; Rust owns only the slot-to-address function.

## Crates

| Crate | Role | Runtime dependencies (allowlist) |
|---|---|---|
| `jorb-vmm` (lib) | Generated Cloud Hypervisor client behind `VmShape`, `boot(socket, shape)` and `state(socket)`. Knows nothing about systemd, slots or Amp | `progenitor-client`, `reqwest` (no default features, `json`), `serde`, `serde_json`, `thiserror`; dev: `tokio`; build: `progenitor` |
| `jorb-config-disk` (lib) | The config disk's size, encoder and parser, shared by both binaries so they cannot disagree | none |
| `jorb` (bin) | Dispatch: env to job, lock, primitives, operations | `jorb-vmm`, `jorb-config-disk`, `tokio` (current-thread), `tracing`, `tracing-subscriber` (json), `anyhow`, `rustix`, `secrecy` |
| `jorb-init` (bin, static musl) | Guest PID 1. Synchronous | `jorb-config-disk`, `rustix`, `tracing`, `tracing-subscriber` (json), `anyhow` |

`jorb-vmm` is the only code that knows Cloud Hypervisor. Its `build.rs` reads the OpenAPI spec from `$CH_OPENAPI_SPEC`, which the flake points at the spec inside the source of the exact Cloud Hypervisor package it installs, so client and binary cannot drift. The generated module stays private.

## Rust conventions

### Toolchain

- Edition 2024; toolchain from the flake. Add dependencies with `cargo add` so versions are current, then read docs.rs for the version you got. Do not rely on remembered APIs for `reqwest::ClientBuilder::unix_socket`, progenitor's builder interface, `rustix::fs::ioctl_ficlone` or `std::fs::File::lock`.
- `unsafe_code = "forbid"` workspace-wide. `rustix` covers the syscalls. If something seems to need `unsafe`, stop and propose.

### Reading order

The code is literate in structure, not in comment volume: it is arranged to be read top to bottom as an explanation.

- Every crate root and module opens with a short `//!` paragraph: what the module is for, what it refuses to know about, and any reasoning the code cannot show. Design prose lives there and nowhere else.
- Order code top-down: the public entry point first, then the functions it calls, in call order. `main` reads as the job's story: parse, lock, observe, converge, report.
- A reader should understand a module from its `//!` paragraph and its function names without opening the bodies.

### Names

- Use the project vocabulary and do not coin synonyms: orb, job, operation, primitive, slot, base image, disk, config disk, gate.
- Identifiers are whole words that say what the thing is: `config_disk`, not `cfg`; `lowest_free_slot`, not `find_avail`. Allowed short forms: `id`, `vm`, `ip`, `api`, `cpu`, `mib`.
- No filler nouns: `Manager`, `Handler`, `Helper`, `Util`, `Service`, `Processor`, `Wrapper`, `Context`, `Info`, `Data`. A type that seems to need one has an unclear job; rethink it.
- A new type or module must remove a state or a duplication. Otherwise write a function.
- Write plain prose in comments, commits and `PROGRESS.md`: what happened and why, in ordinary words. No filler adjectives ("robust", "comprehensive", "seamless"), no emoji, no restating the task.

### Comments

- Comment only what the code cannot say: non-obvious behaviour, an invariant, the reason for a surprising choice, or an `// ASSUMES A<n>` marker (see `PLAN.md`).
- A public item gets a doc line only when its name and signature leave something unsaid: units, failure modes, ownership.
- Delete a comment rather than keep one that repeats the code.

### Interfaces

- Write a module's public signatures before its bodies. Check them against three questions: what must the caller know, what can the caller get wrong, and what does this module refuse to know? If the answer to the second is not "nothing", narrow the types.
- Parse at the edge, once. `OrbId`, `Slot` and `ConfigDisk` are constructed only through fallible parsers; functions take these types, never raw strings.
- No boolean parameters in public functions. Use two methods or an enum.
- Use typestate where it deletes a state. Do not add a handle type that only names something a path already names: `jorb-vmm` takes an API socket everywhere, because a second name for one VM would be a second thing to keep true.
- Errors are part of the interface: `thiserror` enums in `jorb-vmm`; `anyhow` with `.context(...)` in binaries. No `unwrap`, `expect`, `panic!`, `todo!` or `dbg!` outside tests (clippy enforces this).
- No traits, generics or modules for hypothetical future use.
- All `ORB_*` input is untrusted (see Security). Never build a shell string; use `Command` with separate arguments.

### Bounds

- Every loop has a bound visible at the loop: a finite iterator, or a counter checked against a named limit. No `loop` or `while` whose exit depends only on the outside world.
- Every wait has a deadline and every external call a timeout: subprocesses, API requests, socket and interface polling. The job as a whole has a deadline below Amp's five minutes.
- Values the design fixes are named constants, each with a doc line saying what it fixes and why that value: the bounds that keep loops and waits finite, and the names, paths, devices and addresses the interface pins down. Every number in "Fixed interfaces" becomes one, for example `MAX_ORB_ID_LEN`, `jorb_config_disk::BYTES`, `MAX_SLOTS`, `API_SOCKET_WAIT`, `UNIT_STOP_TIMEOUT`, `JOB_DEADLINE`. Named limits are required; a global constants file is not. In `jorb`, durations, counts and sizes live in `limits.rs`, and a name, path, program or device lives in the one module that uses it. Smaller crates keep theirs at the top of the file that uses them. No bare literals for sizes, counts, retries, durations or fixed names elsewhere.
- One exemption: `jorb-init`'s reap loop runs until bootstrap exits. It blocks in `waitpid`, lives in a function returning `!`, and is the only such loop.
- No recursion. If an iterative version would be substantially larger or harder to read, stop and ask, with both versions sketched.

### Logging and secrets

- Logging is `tracing` only, JSON to stderr (`jorb`) or the console (`jorb-init`), with span-close events for timings. Field names are fixed because dashboards query them: `op`, `orb`, `slot`, `outcome`.
- `ORB_TOKEN` is a `secrecy::SecretString` from the moment it is parsed. It never appears in logs, argv, error messages or on persistent disk.

### Tests and size

- Unit-test every pure function. Tests must not need root, KVM or network; those checks are scripts under `tests/host/`.
- Size guidance, to detect over-building rather than as targets: about 150 hand-written lines in `jorb-vmm`, 300 in `jorb`, 150 in `jorb-init`.

## Security

The provider is a closed-source, self-updating CLI. It runs as an unprivileged user and invokes `jorb` through one sudo rule that preserves only `ORB_OPERATION`, `ORB_ID`, `ORB_TOKEN` and `AMP_URL`. `JORB_*` settings are set by the Nix wrapper on the root side and never taken from the caller. Input validation in `jorb` is therefore the privilege boundary.

## Commands

```sh
nix develop                                            # toolchain, cloud-hypervisor, CH_OPENAPI_SPEC
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
nix flake check                                        # all of the above plus Nix evaluation
```

## Working method

- Work one task at a time, in `PLAN.md` order, respecting dependencies. Read the task's acceptance checks before writing code.
- After each task: run its checks, commit (`T<n>: <what>, and why`), and update `PROGRESS.md` with status, the commands you ran and their results, and any open question. Git history plus `PROGRESS.md` is the project's memory across sessions.
- Report faithfully. A check that failed or was not run is recorded as such, with output. Never weaken a check, stub a function or special-case a test to get a pass.
- Checks marked `[host]` need the real machine, which is not the same as needing root. Booting a VM needs only `/dev/kvm`, which is mode 0666 here; root is for creating taps, system-scope `systemd-run`, cgroup reads and writing `/var/lib/jorb`. Work out which of the two a check actually needs before recording it as blocked, and run everything that is not. Otherwise write the script, record "awaiting host run" in `PROGRESS.md`, and continue with tasks that do not depend on the result. Do not infer a gate's outcome.
- Keep going until every remaining task is blocked on a `[host]` result or on an "ask first" item, then report what is done, what is verified, and what is waiting.
- At the end of each milestone, have a reviewer with fresh context (a subagent, if you have one) check the diff against this file's Model, Failure model, Fixed interfaces, Security, Names and Bounds sections. Fix what it finds before moving on.

## Ask first

Stop and ask the human before:

- touching host partitions, secrets, or any NixOS configuration outside the jorb module;
- changing a fixed interface, adding a dependency outside the allowlist, or relaxing the network policy;
- introducing recursion, or a second loop without a visible bound;
- acting on a gate result that contradicts an assumption in `PLAN.md`.
