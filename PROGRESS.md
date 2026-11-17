# Progress

## Status

| Task | State |
|---|---|
| T0 repository skeleton | done |
| T1 guest kernel | building, config check written |
| T2 generated client spike | done, A9 and A10 verified |
| T3 host gates | G1, G2, G8 pass; G3 needs the XFS partition; G4, G5, G7 await a root run |
| T4 jorb-vmm API | done but for the host example |
| T5 jorb-init | not started |
| T6 base image script | not started |
| T7 jorb types | not started |
| T8 jorb primitives | not started |
| T9 NixOS module | not started |
| T10 observability | not started |
| T11 behaviour matrix | not started |
| T12 end to end | not started |

## Gate results

Nothing run yet.

## Open questions

Nothing open.

## Proposals

Nothing proposed.

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
| G3 | FAIL. `/var/lib/jorb` does not exist yet; the XFS partition is the owner's step |
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
