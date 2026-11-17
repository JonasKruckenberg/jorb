# Progress

## Status

| Task | State |
|---|---|
| T0 repository skeleton | done |
| T1 guest kernel | building, config check written |
| T2 generated client spike | done, A9 and A10 verified |
| T3 host gates | not started |
| T4 jorb-vmm API | not started |
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
