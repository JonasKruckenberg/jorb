# Progress

## Status

| Task | State |
|---|---|
| T0 repository skeleton | done |
| T1 guest kernel | not started |
| T2 generated client spike | not started |
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
