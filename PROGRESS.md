# Progress

## Status

| Task | State |
|---|---|
| T0 repository skeleton | done |
| T1 guest kernel | done on aarch64; the x86_64 half is written but unbuilt |
| T2 generated client spike | done. A9 and A10 verified |
| T3 host gates | G1, G2, G6 and G8 pass. G3 needs the XFS partition. G4, G5 and G7 await a run as root |
| T4 jorb-vmm API | done but for the host example, which awaits G4's harness |
| T5 jorb-init | done. The in-guest run awaits root |
| T6 base image script | done. Producing the image awaits root |
| T7 jorb types | done |
| T8 jorb primitives | done. Behaviour is T11's to verify |
| T9 NixOS module | done. `[human]`: wire it and a token file into the host |
| T10 observability | done. Panels returning data awaits T11's run |
| T11 behaviour matrix | written, awaits a run as root |
| T12 end to end | `[human]` |

`nix flake check` passes from a clean checkout (verified by cloning the
repository into a temporary directory and running it there). It covers
formatting, clippy with the workspace's denied lints, the test suite (26 unit
tests), evaluation of a host that enables the module, and `nft --check` over
the network policy that host generates.

What is left is everything that needs root on this machine, plus the owner's
own steps. Nothing is blocked on a decision.

## What to run next, as root

```sh
# The gates that need taps, systemd-run and cgroups, in this order.
sudo -E tests/host/g4-boot.sh
sudo -E tests/host/g5-free-page-reporting.sh
sudo -E tests/host/t5-jorb-init.sh      # T5 in a real guest

# The behaviour matrix, all seven sections, or one at a time.
sudo -E tests/host/matrix.sh
sudo -E tests/host/matrix.sh 4          # just the network policy, say

# Once /var/lib/jorb is an XFS partition:
sudo -E tests/host/g3-reflink.sh
sudo nix run .#jorb-prepare-image       # the real base image, needs the network

# Once the module and T10 are on the host:
sudo -E tests/host/g7-observability.sh
```

The gates read `JORB_*` from the environment, so `sudo -E` rather than plain
`sudo`, and they build what they need from the flake themselves.

## Gate results

| Gate | Result |
|---|---|
| G1 | PASS. 15673 MiB total, 12309 MiB available, so `memoryHigh = 11797M` |
| G2 | PASS. `hpage_pmd_size` is 33554432 (32 MiB), so A6 holds |
| G3 | `/var/lib/jorb` is now an XFS mount, so the owner's step is done. The gate writes a test file into it, so the run itself still needs root |
| G4 | awaiting root |
| G5 | awaiting root |
| G6 | PASS. A7 and A8 hold; digest recorded |
| G7 | awaiting root, and T10's Alloy file has to be installed first |
| G8 | PASS. A12 holds: `nixpkgs#amp-cli` has `orb provider` |

### Assumptions

| ID | State |
|---|---|
| A1 Cloud Hypervisor boots a 4K guest on a 16K host | awaiting G4 |
| A2 disks appear as vda then vdb | awaiting G4. Marked `// ASSUMES A2` in jorb-vmm and jorb-init |
| A3 serial is the architecture's port and reaches the journal | awaiting G4. Marked `// ASSUMES A3` in jorb's limits |
| A4 the guest clock is right at boot | awaiting G4 |
| A5 free page reporting returns memory to the host | awaiting G5. Marked `// ASSUMES A5` in jorb-vmm |
| A6 the host's PMD huge page is 32 MiB | verified by G2 |
| A7 the image has glibc, setpriv, user `user`, /usr/local/bin/amp | verified by G6, with one correction below |
| A8 `amp orb bootstrap` stays in the foreground | verified by G6 |
| A9 progenitor accepts the spec | verified by T2, with two patches |
| A10 a progenitor client works over reqwest's unix socket | verified by T2 against a live VMM |
| A11 Alloy's cAdvisor exporter sees /jorb.slice; the journal reaches Loki | awaiting G7. Marked `ASSUMES A11` in nix/jorb.alloy |
| A12 an `amp` with `orb provider` is installable here | verified by G8 |

A7 needs one correction: `/usr/local/bin/amp` in the image is a symlink to
`/home/user/.amp/bin/amp`, which does exist in the image. jorb-init execs the
symlink, so this works, but the binary behind it is the one Amp replaces on
startup.

## Open questions

1. `AGENTS.md` describes the host as having 8 GiB of memory; this machine
   reports 15673 MiB. The G1 figure above is from the machine, not the
   document.
2. G1 was read with no observability stack running and no zram configured, so
   it is an upper bound rather than the steady state the plan asks about.
   Worth re-reading once T9 and T10 are actually on the host.
3. This session has no passwordless sudo, so G3, G4, G5, G7, T5's in-guest
   run, T6's image build and all of T11 are written but unrun. What they
   actually need is CAP_NET_ADMIN to create the slot taps, system-scope
   systemd-run and cgroup reads, and write access to root-owned
   /var/lib/jorb. Nothing short of root grants that set, so it is a root
   shell or a NOPASSWD rule; a password prompt cannot be answered from a
   non-interactive session.
4. The `latest` tags on the Amp orb images move often: the arm64 digest
   changed between two G6 runs less than an hour apart. T6 pins a digest, and
   refreshing it means running G6 again.
5. x86_64 is prepared throughout but nothing has been built or run on an
   x86_64 host. The guest kernel fragment for it, the PVH boot path and the
   amd64 image digest are all unexercised.

## Proposals

1. **Orb units are now `jorb-orb-<id>.service`.** Accepted by the owner on
   2026-11-18 and done, so this is a record rather than a proposal. The old
   `jorb-<id>` pattern caught two other kinds of unit: `jorb-provider.service`,
   which parsed as an orb called "provider", and the gates' own
   `jorb-gate-<id>` units, which run VMs in `jorb.slice` and whose ids parse
   cleanly, so recover could have stopped a gate mid-run. Both are out of reach
   by name now, and recover no longer asks systemd for each unit's slice to
   tell them apart.
2. **x86_64 hosts.** Added at the owner's request during this session. It
   changes one thing in Fixed interfaces: the cmdline's console is `ttyS0` on
   x86_64 rather than `ttyAMA0`. Everything else is per-architecture Nix.

## Notes per task

### T0

- `clippy::infinite_loop` exists in clippy 0.1.100, so it is denied in
  `[workspace.lints.clippy]`. Checked with `cargo clippy -- -D
  clippy::infinite_loop` against a scratch crate, and against a misspelled lint
  name to be sure an unknown lint would have been reported.
- `nix flake check` runs three checks built from the pinned toolchain: `test`
  (builds the workspace and runs `cargo test`), `clippy` (`--workspace
  --all-targets -D warnings`) and `fmt` (`cargo fmt --all --check`). All pass.
- Verified the lint config is not vacuous: an `unwrap()` in `jorb-vmm`'s library
  code fails clippy, while an `unwrap()` and a `println!` inside a `#[cfg(test)]`
  module pass, which is what `clippy.toml` intends.
- The dev shell also carries tmux, skopeo, umoci, e2fsprogs, util-linux, jq,
  shellcheck, ripgrep, fd, iproute2, nftables and xfsprogs, so the later tasks
  and the host gates do not reach for host tools. `CH_OPENAPI_SPEC` points into
  the source of the same `cloud-hypervisor` package the shell installs (54.1).

### T1

- Cloud Hypervisor 54.1's source tree ships no reference guest kernel config.
  `resources/` holds only a Dockerfile, and `scripts/test-util.sh` builds its
  test kernel with `make ch_defconfig` inside the separate
  `cloud-hypervisor/linux` fork (branch `ch-7.2.8`). Pulling that fork would add
  a second, unpinned source, so the kernel starts from `defconfig` plus
  fragments, which is the plan's fallback.
- The fragments are `nix/guest-kernel.config` (what both architectures need)
  plus `nix/guest-kernel-arm64.config` or `nix/guest-kernel-x86.config`.
  `nix/kernel-config-check.sh` fails the build if the final `.config` does not
  honour every line of them, so a symbol that loses its dependencies cannot
  slip through to a guest that will not boot.
- x86_64 hosts were added at the owner's request. A jorb host boots guests of
  its own architecture, so nothing cross-builds: aarch64 produces
  `arch/arm64/boot/Image`, x86_64 produces the uncompressed `vmlinux` that
  Cloud Hypervisor enters through PVH (`CONFIG_PVH=y`). Both install as
  `$out/kernel`.
- The x86_64 fragment is the plan's required list translated to that
  architecture: 8250 serial with console instead of PL011, CMOS RTC instead of
  PL031, ACPI and MMCONFIG instead of a device-tree PCI host bridge, and no
  page size option because x86_64 is 4K-paged already.

### T2

progenitor 0.16.1 does not accept Cloud Hypervisor 54.1's spec unchanged, so
`build.rs` patches the parsed document. Both patches fail the build instead of
guessing if the spec stops looking the way they expect.

1. 24 operations have no `operationId`, which progenitor rejects outright
   (`path /vmm.ping is missing operation ID`). Each of those paths has exactly
   one method, so the patch names the operation after the path: `/vmm.ping`
   becomes `vmm_ping`.
2. The eight device hot-plug operations declare two success responses, a 200
   carrying `PciDeviceInfo` and a bare 204. progenitor asserts on one response
   type per operation (`assertion failed: response_types.len() <= 1`), so the
   patch drops the empty one. jorb hot-plugs nothing.

- A9 verified: `cargo build -p jorb-vmm` succeeds with those two patches.
- A10 verified on the host: `cloud-hypervisor --api-socket /tmp/jorb-t2/api.sock`
  plus `cargo run -p jorb-vmm --example ping -- /tmp/jorb-t2/api.sock` printed
  `54.1.0`, which matches `cloud-hypervisor --version` (v54.1.0). reqwest
  0.13.5's `ClientBuilder::unix_socket` needs no feature flag, only `cfg(unix)`.
- Worth knowing for T8: Cloud Hypervisor refuses an API socket path longer than
  `SUN_LEN` (108 bytes) with "path must be shorter than SUN_LEN". The fixed
  interface `/run/jorb/<id>.sock` is well inside that, but a test that puts
  sockets under a long temporary directory is not.
- `build.rs` needs two build-dependencies beyond progenitor itself: `openapiv3`
  2.2.0 to name the spec's type (progenitor does not re-export it) and
  `serde_yaml` 0.9 because the spec is YAML. Both versions are the ones
  progenitor already uses. No `syn` or `prettyplease`: the generated tokens go
  to `$OUT_DIR` unformatted.

### T3

Eight gate scripts in `tests/host/`, each printing one PASS or FAIL line plus
evidence, sharing `lib.sh`. `lib.sh` also holds the VM shape as Cloud
Hypervisor's API wants it, so G4 and G5 boot the same shape from the same
definition, by hand with curl, independently of jorb's own code. The busybox
ext4 image G4 and G5 boot is `nix build .#test-image`, and its init
(`nix/test-init.sh`) prints one `JORB-GATE` marker per thing a gate observes.

Run on this host (aarch64, 16K pages, /dev/kvm present, no root in this
session):

| Gate | Result |
|---|---|
| G1 | PASS. 15673 MiB total, 12309 MiB available, so `memoryHigh = 11797M` |
| G2 | PASS. `hpage_pmd_size` is 33554432 (32 MiB), so A6 holds |
| G3 | `/var/lib/jorb` is XFS as of 2026-11-18; the gate still needs root to write into it |
| G4 | awaiting a run as root (taps, systemd-run, cgroups) |
| G5 | awaiting a run as root |
| G6 | see below |
| G7 | awaiting a run as root, and it needs T10's Alloy config installed first |
| G8 | PASS. A12 holds: `nixpkgs#amp-cli` really has `orb provider` |

Notes:

- G1's number was read with no observability stack running and no zram
  configured, so it is an upper bound rather than the steady state the plan
  asks for. Worth re-reading once T9 and T10 are on the host. The host also has
  15.7 GiB of memory, not the 8 GiB `AGENTS.md` describes.
- G8: `nixpkgs#amp` is a text editor that shares the name; the gate now builds
  each candidate and runs `amp orb provider --help` rather than trusting the
  attribute name. `nixpkgs#amp-cli` is unfree, so whatever installs it has to
  allow that.
- The Amp orb images, from https://ampcode.com/docs/orbs/self-hosted:
  `us-central1-docker.pkg.dev/amp-prod-0/amp-orb/amp-orb-arm64:latest` for
  aarch64 and `.../amp-orb:latest` for x86_64. The arm64 digest at the time of
  writing is
  `sha256:c231df95186127aea0c68fd3642323629a0e65157b275e98d5d16e89ed1e0805`.
- The same page lists the hosts an orb needs to reach: `ampcode.com:443`,
  `static.ampcode.com:443`, `tunnel.ampcode.com:443`, git hosts on 443 and
  `storage.googleapis.com:443`. T9's network policy has to let DNS out too,
  because the cmdline points the guest at 1.1.1.1 and 9.9.9.9.

### T4

- `VmBuilder::new` takes the kernel, the cmdline, the vCPU count and the memory
  size, because a VM without the last two is not a shape anyone meant to ask
  for. Their types differ (`NonZeroU8` and `NonZeroU32`) so the two numbers
  cannot be swapped by mistake.
- Serial on stdout and the virtio console off are not methods: the fixed
  interface has them always, and a builder that could leave serial off would be
  a builder that can produce a guest whose output never reaches the journal.
- `VmState` is this crate's own enum, with one variant Cloud Hypervisor does
  not have: `Absent`, for a VMM that answers but has no VM. That is what a job
  killed between `systemd-run` and `vm.create` leaves behind, and making it a
  state rather than an error keeps `state()` total.
- The golden file is `jorb-vmm/tests/vm-shape.json`, compared against what the
  builder serialises for the v1 sizing. It includes the fields Cloud
  Hypervisor's types default (`queue_size`, `hotplug_method`, `vhost_mode` and
  so on), because those are part of what jorb actually sends.
- `cargo test -p jorb-vmm` passes, clippy is clean, `cargo fmt --check` is
  clean. The `[host]` half of T4's acceptance is `examples/boot.rs`, which
  boots the G4 image through the builder; it is awaiting the same root run as
  G4.

### T6

- `nix/image.nix` pins a digest per architecture and wraps
  `nix/prepare-image.sh`, which is a script you can read on its own: the
  wrapper passes it the repository, the digest, the jorb-init path and the
  image size, and the body names them at the top.
- Digests recorded on 2026-11-18: arm64
  `sha256:e25e30674cc051a6477fe9b148da1627f4186828dbd18e3151deeaa0d8ce1e68`,
  amd64
  `sha256:a431746a91926c0686942b73ab0ccfa03e23eb96f5a647612e5948ec4e68729b`.
  The `latest` tags move often: the arm64 digest changed between two G6 runs
  less than an hour apart, which is the reason the plan pins one.
- `skopeo copy` needs `--insecure-policy` on a host with no
  `/etc/containers/policy.json`. The digest is the integrity check, and it is
  the thing being pinned, so a signature policy would add nothing here.
- `nix build .#jorb-prepare-image` passes, which runs shellcheck over the
  combined script. Producing the base image itself needs root and is awaiting
  a host run.

### T7

24 unit tests, including every rejection the plan asks for. Two notes:

- `jorb` uses `anyhow` rather than error enums, as AGENTS.md's crate table
  says, so the refusals are messages and the tests assert on them.
- `ConfigDisk::new` takes references, because `secrecy::SecretString` is not
  `Clone` by design, and copying the secret once at the edge is clearer than
  threading an owned token through the operations.

### T8

- A near-miss worth recording: `recover` lists `jorb-*.service`, and the
  provider's own unit is `jorb-provider.service`, which parses as an orb
  called "provider". Reading the name alone, recover would have stopped the
  provider that invoked it. It now asks systemd for each unit's slice and
  only touches units in `jorb.slice`; the provider runs in `system.slice`.
  There is a test for the name parsing and a comment at the check.
- Every `systemctl` and `systemd-run` call passes `--no-ask-password`:
  jorb runs from a service, and a polkit prompt would be an unbounded wait.
- The lock is `File::try_lock` in a bounded loop rather than a blocking
  `lock`, so the wait has a visible deadline like every other wait.
- Smoke-tested unprivileged, against an empty state directory:

  | Job | Result |
  |---|---|
  | `pause` on an absent orb | exit 0, `state_before=Absent state_after=Absent` |
  | `delete` on an absent orb | exit 0 |
  | `spawn` with no token | exit 1, "spawn needs ORB_TOKEN, and this job has none" |
  | `spawn` with a token, no base image | exit 1, "opening the base image at ...; has jorb-prepare-image run?" |

  The token appeared in zero log lines across those runs.

### T9

- `nix/module.nix` holds the whole host side; `nixosModules.default` in the
  flake fills in this flake's own builds as the defaults for `package`,
  `prepareImagePackage` and `kernel`.
- The privilege boundary, as generated and checked:

  ```
  jorb ALL=(ALL:ALL) NOPASSWD:SETENV: /nix/store/.../jorb-root/bin/jorb-root
  Defaults:jorb env_keep += "ORB_OPERATION ORB_ID ORB_TOKEN AMP_URL"
  ```

  One program, four variables. `jorb-root` then sets every `JORB_*` itself, so
  nothing the provider says can describe the host.
- The provider unit is pinned to `system.slice`, which is the other half of
  the recover near-miss above: `jorb-provider.service` matches the pattern
  recover looks for, and the slice is what tells them apart.
- `checks.module` evaluates a host with the module enabled and then checks the
  generated nftables ruleset with `nft --check`. That needs a netlink socket,
  which root does not have to provide: `unshare --map-root-user --net` gives
  the checker a private network namespace, and it works inside the Nix
  sandbox. Both of T9's acceptance checks therefore run in `nix flake check`.
- `[human]` still to do: wire the module and a token file into the real host
  configuration, and create the XFS partition G3 wants.

### T10

- `nix/jorb.alloy` is installed as its own file in Alloy's configuration
  directory, with the remote-write endpoint substituted from a module option.
  It keeps only series whose cgroup is under `/jorb.slice` and turns
  `/jorb.slice/jorb-<id>.service` into `orb="<id>"`, which is the label every
  panel groups by.
- `nix/jorb-dashboard.json` is provisioned from its own directory, so the
  owner's dashboards are untouched. Panels: CPU, memory and IO per orb; tap
  bytes per slot; zram original against compressed; job duration by operation
  and job outcomes from the JSON logs; a "no free slot" count; and the failed
  jobs themselves. The datasources are dashboard variables, so it does not
  assume what the owner's are called.
- The module check also asserts the dashboard is JSON with at least one panel
  and that every non-row panel carries a query.
- `[host]` every panel returning data is T11's run, which is still awaiting
  root. The zram panel also needs node_exporter's zram collector enabled, and
  the tap panel its netdev collector.

### T11

- `tests/host/matrix.sh` runs the seven sections, each printing its own PASS
  or FAIL line plus evidence, with one summary line at the end. It takes
  section numbers as arguments so a single section can be rerun.
- An orb's state is read from outside jorb: `systemctl is-active`, the VMM's
  own `vm.info` over the socket, and whether the disk file is there. That way
  a section can fail even if jorb's own idea of the state is wrong.
- Section 4 needs probes from inside the guest, and the guest cannot work out
  the host's LAN address by itself. The matrix writes those two addresses into
  its own copy of the base image with `debugfs`, and the test image's
  stand-in for `amp orb bootstrap` probes them and prints the results. jorb
  has no hooks for any of this: the scaffolding is all inside the test image.
- Section 5 picks the memory workload with the token it spawns with, because
  the token is the only per-job value the matrix can choose.
- Three bugs found by rereading it before running anything: the wipe between
  sections deleted its own base image, a `[ ... ] && x=y` would have exited
  the script under `set -e` for pause and delete, and `pause` on an orb that
  never existed was expected to leave a disk behind.
- Awaiting a run as root. It is the only thing that can turn T8's "behaviour
  verified in T11" into a result.

## Simplification pass, 2026-11-18

Asked for by the owner after T11: fewer states, fewer types, clearer module
boundaries, tests only where they earn their place. No task's behaviour
changed except the two items marked below. `nix flake check` passes.

What came out, and why each was not pulling its weight:

- **`jorb-vmm`'s `Vm` handle.** "Absent" had two representations: `attach`
  returned `Option<Vm>` and `VmState` had an `Absent` variant, so every
  observation was a ping followed by a `vm.info`. The typestate it was meant
  to provide was not true either, because `attach` minted a `Vm` for a VM that
  did not exist. The crate is now `VmBuilder::boot(socket) -> Result<()>` and
  `state(socket) -> Result<VmState>`, where an API socket is the only name for
  a VM and `Absent` is the single answer for a socket that is gone, dead or
  has no VM. One request per observation instead of two.
- **`Paths`.** Six fields of which three were host-wide, not per-orb, so the
  same lock and base image arrived under an orb's name, and `disks_directory`
  carried an `unwrap_or` for a case that cannot happen. `Settings` already
  holds the two directories, so the layout is now pure functions in `host.rs`
  over `&Settings`, with the orb passed where it matters.
- **`run_allowing_failure` and `run_to_completion`.** The first existed only
  because `systemctl is-active` exits non-zero for an inactive unit.
  `systemctl show --property=ActiveState --value` reports `inactive` and exits
  0 even for a unit systemd has never heard of (checked on this host), so
  there is one `run` and no second meaning for a non-zero exit.
- **`unit_slice`.** Only needed to tell `jorb-provider.service` from an orb's
  unit. The `jorb-orb-` prefix does that by construction, so recover makes one
  fewer subprocess call per unit per job.
- **`ConfigDisk::encode`.** `new` validated and `encode` serialised, leaving a
  validated-but-unencoded state and a second copy of the token in memory.
  `ConfigDisk` now holds the padded block itself, with one fallible
  constructor and a hand-written `Debug` that never prints it. The two keys
  are a table the constructor walks, so the newline-and-NUL refusal is written
  once instead of per key, and a third key would be one line.
- **`Slot::netmask`**, a method that ignored `self` and returned the same /30
  for every slot. Now `limits::GUEST_NETMASK`.
- **`Outcome.changed`**, which was `before != after` stored next to `before`
  and `after`. The struct is `Transition` now, so it stops colliding with the
  `outcome` log field, which means something else.
- **`examples/ping.rs`.** It was `state()` spelled out by hand; `examples/boot.rs`
  exercises the same transport and more, and the gates query `vm.info` with
  curl on purpose, independently of jorb's own code.
- **`Job`** is `Request`, at the owner's request: too close to `jorb` to read
  quickly.
- **`tokio` in `jorb-vmm`** is a dev-dependency. Nothing in the library used
  it; the generated client uses `reqwest` and `serde_json` only.

Smaller things, from a second round aimed at code size rather than structure:

- `systemd-run` takes `--property=NAME=VALUE` as one argument, so the three
  properties stop being six array entries.
- `--api-socket=` likewise, which also drops one `to_string_lossy`.
- `ensure_disk`'s error contexts named both paths in two of its messages,
  which rustfmt then spread over four lines each. They name the one path that
  identifies the failure now.
- `jorb-vmm` had `1024 * 1024` inline; it is `limits::BYTES_PER_MIB`, which is
  what the conventions ask for.

One thing tried and reverted, recorded so it is not tried again without the
argument being better: replacing `VmBuilder` with a `VmShape` struct of
exactly the fixed interface, on the grounds that each of its six methods has
one caller and that `without_huge_pages` and `free_page_reporting` are never
really optional. The owner's call is that the builder stays.

Four things that were dead or wrong, found by reading rather than by running:

- `start` called `Span::current().record("slot", ...)` on a field the
  `#[instrument]` attribute never declared, so the slot never reached the
  span. tracing drops a record for an undeclared field without complaining.
  The field is declared now, so the dashboard's `slot` is on the span as well
  as the two events.
- `lowest_free_slot` carried `u8::try_from(index).context("a slot index past
  255")` on an index that cannot reach 256. The loop is `(0..=u8::MAX).take(n)`
  now, which is u8-typed and still shows its bound.
- **Behaviour change.** `jorb-init` read `/etc/jorb-env` with
  `unwrap_or_default()`, so an image without it booted on with no `PATH`.
  AGENTS.md's step 5 says a failed setup step powers off, and
  jorb-prepare-image always writes the file, so its absence means this is not
  the image jorb built. It is an error now.
- **Behaviour change.** Spawn observed the VM state again after converge had
  just observed it under the same lock. Converge passes what it saw.

Tests went from 41 to 26. Nothing's coverage was dropped: the ones that went
were strictly subsumed (256 distinct slot networks, which tested
`Ipv4Addr::new`; two identical config disks being byte-equal, which the exact
byte assertion already proves; disk ordering, which the golden JSON carries),
and the rest were merged into tables where they had been one test per input.

Sizes after the pass, counting lines that are neither blank, comment nor
test: `jorb` 601 against the guidance's 300, `jorb-vmm` 248 against 150,
`jorb-init` 166 against 150. All three are still over, and the remaining
excess is code rather than prose: `jorb`'s eighteen functions, and
`jorb-vmm`'s builder plus its error enum. The honest reading is that the
guidance is measuring something real and two rounds of deleting indirection
did not reach it, so the next move is structural rather than cosmetic. Worth a
second pair of eyes on whether `jorb` wants splitting.

## Review

AGENTS.md asks for a reviewer with fresh context at the end of a milestone.
This session was told not to use subagents, so the review was done by
rereading the diff against AGENTS.md's Model, Failure model, Fixed
interfaces, Security, Names and Bounds sections. It found two things, both
fixed and both recorded above: the provider's unit name colliding with the
orbs' pattern in `recover`, and a second unbounded loop in `jorb-init`'s
`power_off` fallback, which is now `exit(1)` and so leaves the reap loop as
the only one.

The simplification pass above was reviewed the same way, against the same
sections. The unit rename is the part that reaches outside Rust: Alloy's
relabel regex, the dashboard's `orb` label, G1's pattern and matrix.sh all
had to move with it, and matrix.sh lost the `grep -v jorb-provider.service`
it needed twice.
