# Progress

Current state only; how it got here is in git (`git log --stat`), including
the external review of `f95635c` (commits P0 through V5) and the
simplification pass after it.

## Status

Built: T0–T11 (`jorb`, `jorb-init`, `jorb-config-disk`, the guest kernel, the
base image script, the NixOS module, observability, the host matrix). What is
left is `PLAN.md`'s "Remaining": all of it needs the host or the owner.

**Not yet observed on the host:** anything since the review. jorb was
restructured, the config disk is now JSON (`jorb-config-disk`, serde), and the
matrix and its stand-in bootstrap were rewritten. Boot still goes through
Cloud Hypervisor's API via `jorb-vmm`; a short-lived change to booting from
the command line was reverted at the owner's request, because it traded the
generated, typed client for an unchecked string contract. Matrix section 4
also carries the in-guest checks the deleted gate scripts used to make.

**How checks run off the host.** This repository is also worked on from an
aarch64-darwin laptop with no Linux builder, where `nix flake check` skips
`aarch64-linux` and so proves nothing. There, fmt, clippy and the tests run
in an aarch64 Linux container, every flake output is evaluated (`nix eval`
of each `drvPath`; the module check's evaluation runs the NixOS assertions),
and the rendered `inet jorb` table passes `nft --check` in a container.
At the last commit: 11 tests pass, every output evaluates, the table checks,
shellcheck is clean.

## Gate results

| Gate | Result |
|---|---|
| G1 memory | 15673 MiB total, 12309 MiB available with no observability stack running |
| G2 huge pages | `hpage_pmd_size` 32 MiB: A6 holds |
| G3 reflink | PASS (owner): XFS on a loop device, `cp --reflink=always` works |
| G4 boot through systemd | PASS (owner): tap, `ip=` address, journal; A3 holds |
| G5 free page reporting | PASS (owner): A5 holds |
| G6 Amp image | PASS: A7 and A8 hold, digest pinned in `nix/image.nix`. `/usr/local/bin/amp` is a symlink into `/home/user/.amp`, which Amp replaces on startup |
| G7 observability | awaiting a host run with the module installed |
| G8 amp-cli | `nixpkgs#amp-cli` has `orb provider` and is unfree: A12 holds |
| boot-check (script since deleted) | PASS: A1, A2, A4 hold |

A1–A8 and A12 are verified; A11 awaits the host.

## Open questions

1. `[owner]` v1's sizing (1 slot, 6 GiB per orb) was chosen against 8 GiB; the
   host has 16. Keep it?
2. `[owner]` A spawn restarts a running orb unless the config disk is
   equal, so a new token restarts it. If Amp mints a token per spawn
   request and redelivers a spawn for a healthy orb, that restart loses the
   orb's in-memory state. Whether Amp does either is not documented. Matrix
   section 3 tests the current behaviour.
3. The Amp image's `latest` tag moves within hours; `nix/image.nix` says how
   to refresh the pin. Its check script is deleted, so a new pin is verified
   by the end-to-end run.

## Proposals

1. **`panic=-1` on the guest cmdline.** If jorb-init's power-off syscall ever
   fails, the guest halts with the VMM still reporting `Running`. With
   `panic=-1` the kernel reboots instead; in Cloud Hypervisor's source a
   guest reset reboots the VM rather than exiting, so the orb would come
   back and run bootstrap again. That is better than a dead orb that looks
   healthy, but it is a fixed-interface change.
2. **Discard on the orb's root.** Nothing passes discards from the guest, so
   orb disks only grow. Mounting the guest root with `discard` is a cmdline
   change; the alternative is alerting on free space.
