# Progress

## Status

| Task | State |
|---|---|
| T0 repository skeleton | done |
| T1 guest kernel | done (aarch64 is the only platform since S2) |
| T2 generated client spike | done. A9 and A10 verified |
| T3 host gates | G1, G2, G6 and G8 pass. G3 needs the XFS partition. G4, G5 and G7 await a run as root |
| T4 jorb-vmm API | done but for the host example, which awaits G4's harness |
| T5 jorb-init | done. The in-guest run awaits root |
| T6 base image script | done. Producing the image awaits root |
| T7 jorb types | done |
| T8 jorb primitives | done. Behaviour is T11's to verify |
| T9 NixOS module | done. The host's state is the module's now; `[human]`: import it and provide a token file |
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

## What to run next

Everything below runs from the repository root, inside `nix develop`. The
scripts build what they need from the flake themselves.

**No privileges.** Start here; it takes about fifteen seconds and it is the
check that catches a config Cloud Hypervisor will not accept:

```sh
tests/host/boot-check.sh
```

**As root.** These need taps, system-scope `systemd-run` and cgroups, and they
read `JORB_*` from the environment, so `sudo -E` rather than plain `sudo`:

```sh
sudo -E tests/host/boot-under-systemd.sh             # the tap, the ip= address, the journal
sudo -E tests/host/free-page-reporting.sh
```

**The real base image**, which needs the network and root, and has to exist
before anything boots an actual orb:

```sh
sudo nix run .#jorb-prepare-image
sudo -E tests/host/jorb-init-in-guest.sh        # jorb-init in a real guest
```

**The behaviour matrix**, all seven sections or one at a time. Sections 4 and 5 need things this host does not have yet: the module's nftables policy and NAT, and a zram device.

```sh
sudo -E tests/host/matrix.sh
sudo -E tests/host/matrix.sh 4            # just the network policy, say
```

**Once the module and T10 are on the host:**

```sh
sudo -E tests/host/observability.sh
```

**When the pinned Amp digest is bumped**, to re-verify A7 and A8 against the
new image:

```sh
tests/host/amp-image.sh
```

## Gate results

| Gate | Result |
|---|---|
| boot-check | PASS, unprivileged, 2026-11-18. The config jorb sends boots; guest up in 14s, `/dev/vdb` is the config disk, clock within 1s |
| G3 | PASS, reported by the owner 2026-11-18. XFS on /dev/loop0 and `cp --reflink=always` works |
| G4 | PASS, reported by the owner 2026-11-18. With A1, A2 and A4 already settled by boot-check, that leaves the tap, the `ip=` address and the journal, so A3 holds in full |
| G5 | PASS, reported by the owner 2026-11-18, so A5 holds |
| G6 | PASS. A7 and A8 hold; digest recorded |
| G7 | awaiting root, and T10's Alloy file has to be installed first |

Three gates were deleted on 2026-11-18, their answers being settled and
unlikely to change. Keeping the answers and dropping the scripts:

| Deleted | Answer it recorded |
|---|---|
| G1 free memory | 15673 MiB total, 12309 MiB available, so `memoryHigh = 11797M`. The derivation now lives in the module's `memoryHigh` description |
| G2 huge page size | `hpage_pmd_size` is 33554432 (32 MiB), so A6 holds |
| G8 amp-cli | A12 holds: `nixpkgs#amp-cli` has `orb provider`, and it is unfree |

### Assumptions

| ID | State |
|---|---|
| A1 Cloud Hypervisor boots a 4K guest on a 16K host | verified 2026-11-18, unprivileged. See "The 400" below |
| A2 disks appear as vda then vdb | verified 2026-11-18: the guest read `JORB_GATE=...` back off /dev/vdb |
| A3 serial is the architecture's port and reaches the journal | verified: boot-check saw the output on the VMM's stdout, G4 saw it in `journalctl -u` |
| A4 the guest clock is right at boot | verified 2026-11-18: guest 09:28:21 against host 09:29:00 |
| A5 free page reporting returns memory to the host | verified by G5, 2026-11-18 |
| A6 the host's PMD huge page is 32 MiB | verified by G2, since deleted |
| A7 the image has glibc, setpriv, user `user`, /usr/local/bin/amp | verified by G6, with one correction below |
| A8 `amp orb bootstrap` stays in the foreground | verified by G6 |
| A9 progenitor accepts the spec | verified by T2, with two patches |
| A10 a progenitor client works over reqwest's unix socket | verified by T2 against a live VMM |
| A11 Alloy's cAdvisor exporter sees /jorb.slice; the journal reaches Loki | awaiting G7. Marked `ASSUMES A11` in nix/jorb.alloy |
| A12 an `amp` with `orb provider` is installable here | verified by G8, since deleted |

A7 needs one correction: `/usr/local/bin/amp` in the image is a symlink to
`/home/user/.amp/bin/amp`, which does exist in the image. jorb-init execs the
symlink, so this works, but the binary behind it is the one Amp replaces on
startup.

## The 400, and what it cost

Running the gates as root failed with `curl: (22) The requested URL returned
error: 400`. Traced on 2026-11-18. Cloud Hypervisor 54.1 answers `vm.boot`
with

```
["Error from API","The VM could not boot","Failed to validate config","Image type required for disk"]
```

unless every disk carries `image_type`. `DiskConfig.image_type` is an
`ImageType` (`FlatVmdk`, `FixedVhd`, `Qcow2`, `Raw`, `Vhdx`, `Unknown`) with no
default, and every disk jorb attaches is a raw file: the base image, its
reflink clones and the config disk. Fixed in `jorb-vmm`'s disk builder, in the
golden file, and in `lib.sh`'s `vm_config`.

Three things worth keeping in mind from it:

1. **This was jorb's bug, not the harness's.** The same field was missing from
   what `jorb-vmm` sends, so every spawn would have failed the same way. The
   golden test passed throughout: it compares what jorb sends against a
   checked-in copy of what jorb sends, so it catches drift and cannot catch a
   config that no VMM will accept.
2. **The harness threw the answer away.** `start_test_vm` piped both API calls
   to `/dev/null`, so the only thing left was curl's exit code. The body is
   kept now and printed as evidence on failure.
3. **The boot path never needed root.** `/dev/kvm` is mode 0666 on this host,
   and `vm.create` plus `vm.boot` need no privilege beyond that: what needs
   root is the tap, system-scope `systemd-run`, cgroup reads and writing
   root-owned `/var/lib/jorb`. Booting the real kernel and the real test image
   with the net section removed therefore runs unprivileged, which is how A1,
   A2 and A4 were verified without a root shell. They had been sitting behind
   "awaiting root" for no reason.

What that run proved, with the real guest kernel and the real test image:

| Fact | Evidence |
|---|---|
| A 4K guest boots on this 16K host | guest registered HugeTLB at 64 KiB, 2 MiB, 32 MiB and 1 GiB, which is the 4K-granule set; host `getconf PAGESIZE` is 16384 |
| Disks arrive in attach order | `/dev/vda` and `/dev/vdb` present, `/dev/vdc` absent, and `vdb`'s first line was the config disk's |
| The clock is right at boot | guest printed 2026-11-18 09:28:21 UTC, host was at 09:29:00 |
| The serial line carries the guest's output | the whole boot, from `Booting Linux on physical CPU 0x0` to the init markers |
| jorb's own config boots, not just the shell's | the golden file itself, paths substituted and `net` removed, booted to the same markers |

The new check found a second bug on its first run, which is the argument for
having it. A serial console terminates every line CRLF, so `marker` returned
`JORB_GATE=boot-check\r` and the comparison against `JORB_GATE=boot-check`
failed while both sides printed identically in the evidence. G4's A2 assertion
reads the same marker the same way, so it would have failed the same way on its
first root run, and the evidence would have shown two strings that looked
equal. `marker` strips carriage returns now.

Still needing root: the tap and therefore all of section 4's network policy,
G5's free page reporting, G7's observability, and the journal half of A3.

## What the first root run found

G3, G4 and G5 passed. T5 and the matrix did not, both for reasons in the
harness rather than in jorb.

**T5: one console, two writers.** jorb-init's first JSON event came out of the
journal with a kernel message stuck to the end of it:

```
{...,"target":"jorb_init"}[    0.897863] Freeing unused kernel memory: 4096K
```

so `jq` refused the line. The guest kernel and the guest's userspace share one
serial console by design, and printk writes straight to the UART while
jorb-init writes through the tty, so a kernel message can land inside a line.
It is not even unlucky: "Freeing unused kernel memory" is flushed as init
starts, which is exactly when jorb-init logs. Nothing can be done at the
writing end, because the message was already queued before jorb-init ran, and
quieting printk would cost the boot diagnostics that A3 exists to provide. So
the reading end copes: `json_event` in `lib.sh` takes the braces out of the
line and ignores whatever the kernel put either side of them.

The dashboards are unaffected. Every Loki query in `nix/jorb-dashboard.json`
reads `{unit="$provider_unit"}`, which is jorb's own host-side log, not a
guest's console. A panel added over orb units later would need the same care.

**The matrix: reflinks on the wrong filesystem.** It defaulted its state
directory to `/var/tmp/jorb-matrix`, and `/var/tmp` is ext4 on this host, so
`ioctl_ficlone` fails with EOPNOTSUPP and every `ensure_disk` fails with it.
The symptom was `absent/spawn exited non-zero` with a `.tmp` left behind, and
then an apparent hang, which was all seven sections waiting out their
90-second boot deadlines for orbs that could never start. It defaults to
`/var/lib/jorb/matrix` now, on the XFS mount, and `setup` probes for reflinks
up front so the next filesystem surprise says so instead of arriving as a bare
exit code. There is also a guard against pointing `JORB_MATRIX_DIR` at
`/var/lib/jorb` itself, which `setup` would wipe.

Two matrix sections still cannot pass on this host, and neither is jorb's
fault: section 4 wants the nftables policy and the NAT that `nix/module.nix`
installs, and T9 is still the owner's `[human]` step; section 5 wants a zram
device, and there is none configured.

A latent bug in the same file, found while fixing the above: `marker` ran a
pipeline under `set -o pipefail`, so a missing marker killed the script
instead of letting the caller's own check report it. Every gate that reads a
marker was one absent line away from dying without a message.

## The second root run: one missing image step, four symptoms

T5 reported `amp ran as :`, and the matrix failed sections 4, 5 and 7. One
cause under most of it: **the test image had no busybox applet symlinks.**

`nix/test-init.sh` ran `busybox --install -s /bin` as its first act, so a
guest booted `init=/init` had a userland. A guest booted `init=/jorb-init`,
which is every orb and all of T5, had `/bin/sh` and `/usr/bin/setpriv` and
nothing else. Nothing said so, because every missing applet failed quietly:

| Symptom | What was actually missing |
|---|---|
| T5: `amp ran as :` | `id`, so `$(id -u):$(id -g)` was `:` |
| Section 4: every target "unreachable", `ampcode.com` "UNREACHABLE" | `ping` and `wget`. The probes could not tell a policy that blocks from a tool that does not exist, so the section's results meant nothing either way |
| Section 4: no `reach tap` line at all | `ip`, so the guest never worked out its own address |
| Section 5: peak 156 MiB rather than a gigabyte | `dd`, so the workload never allocated |
| Sections 4, 5, 7: "never reported itself ready" | `sleep`. The stand-in ends in `while true; do sleep 3600; done`, which without `sleep` is a hot spin printing "not found" to the serial console as fast as it can, so the journal the matrix greps was being flooded |

The applets are symlinked at image build time now, and `test-init.sh`'s
runtime `--install` is gone: one mechanism, and it covers both init paths.

Three further things, each its own bug:

- **`marker` could be polluted from either side.** The hardening for the
  carriage return was not enough: a kernel message can land in front of a
  marker as well as behind it. boot-check's own evidence showed it
  (`booted 6.18.63 aarch64[   13.102431] Freeing unused kernel memory`). Any
  exact comparison against a marker was one interleaved printk from failing,
  which includes G4's A2 check. `marker` now drops a kernel message on either
  side, by its `[   13.102431]` timestamp.
- **Section 5 waited for the wrong marker.** It waited for `amp ready`, which
  the stand-in prints *after* running the memory workload, so it started
  watching `memory.current` once the gigabyte had already been freed. It waits
  for `amp memory allocating` now.
- **Section 7 asserted something the model forbids.** It killed a spawn of
  `m7a` "once its tap exists but before its VM is running", but that window is
  a few microseconds wide, so what it actually caught every time was a
  *healthy* orb. A healthy orb is left alone, and with one slot that means a
  spawn of `m7b` must fail with the capacity error, which the section then
  recorded as a failure. It had also never freed the slot before expecting
  `m7b` to take it, so the second half could not pass either. Rewritten to
  test what its title promises: the VMM is killed under a running orb, the
  next spawn brings it back on the same disk inode, a second orb is refused
  while the first holds the only slot, and it starts once the slot is freed.
  Killing jobs at arbitrary points is section 2's job, and section 2 passes.
- **The kill no longer matched.** Section 7 found its VMM with
  `pkill -f "api-socket $socket"`, and the systemd-run invocation now spells
  that `--api-socket=<path>` as one argument, so the pattern matched nothing.
  It matches the socket path alone now, which does not depend on the spelling.

Sections 4 and 5 cannot pass until the module is on the host: section 4 needs
the `inet jorb` nftables table and the NAT that comes with it, and section 5
needs the zram device `zramSwap.enable` provides. Both now check for that and
say so rather than reporting it as jorb's failure.

Nothing in this round was a bug in jorb itself. Sections 1, 2, 3 and 6 passed
throughout, which covers every operation from every start state twice, fifty
killed jobs converging, the identical-spawn no-op, and a VMM that never opens
its socket.

## Why T5 failed twice with the same log line

The second T5 run reported the same event as the first, down to the
microsecond and the pid: `{"timestamp":"2026-11-18T10:11:42.596677Z",...,"pid":105}`,
with "bootstrap reached after 0s". A guest does not boot in zero seconds. It
was reading the *previous* run's journal, so the applet fix above had never
been looked at.

`lib.sh` built its journal window as `date -u '+%Y-%m-%d %H:%M:%S'` and passed
it to `journalctl --since`. `date -u` prints UTC; `journalctl --since` reads a
bare timestamp as **local** time. This host is Europe/Berlin, UTC+1, so the
window opened an hour before the run started and swallowed everything the
previous run had logged. The gate then matched a stale marker on its first
poll.

A time window was the wrong tool anyway. Both `lib.sh` and the matrix now read
`journalctl _SYSTEMD_INVOCATION_ID=<id>`, taken from
`systemctl show -p InvocationID`, which means exactly "this start of this
unit" and involves no clocks, timezones or formats.

The matrix had the same exposure for a different reason: `guest_log` filtered
by unit alone, and orb names repeat between runs (`m4`, `m5`, `m7a`), so a
section could match its own marker from an earlier run before the orb it was
waiting on had booted. That is very likely part of why sections 4 and 5 read
as "never reported itself ready".

**T5's substance is verified**, unprivileged, by booting the image with
`init=/jorb-init` and a token on the config disk, outside systemd:

| Step | Marker |
|---|---|
| 6, setpriv dropped to the image's user | `amp user 1000:1000`, previously `:` |
| 4, the config disk's keys reached the child | `amp token present` |
| 3, /proc/net/pnp copied | `amp resolv #MANUAL nameserver 1.1.1.1 nameserver 9.9.9.9` |
| 2, the writeback window | `amp dirty 100/100` |
| 1, the mounts | `amp mounts / /dev /proc /sys /dev/pts /dev/shm /run` |

The `ip=` cmdline fills `/proc/net/pnp` with its resolvers even when there is
no interface to configure, which is why step 3 can be checked without a tap.
What the root run still adds is that those lines reach the unit's journal,
which G4 has already shown separately.

Section 7 passes as rewritten.

## Where this is going, decided 2026-11-18

Read the self-hosted orbs documentation again before deciding any of this. Two
things in it change the picture:

1. **Amp supplies a Helm chart for GKE, EKS and AKS**, which "runs the orb
   provider and a dispatch program that creates orbs as pods and saves paused
   orbs to your storage bucket, so you do not write a dispatch program
   yourself." So jorb is not the way to get self-hosted orbs on a cloud. If
   that is the goal, the chart is less work and is maintained by Amp.
2. **The capability matrix.** Amp-hosted orbs have memory, disk and project
   snapshots; the Helm chart has disk and snapshots but not memory. jorb has
   disk only: pause is a power cut, so no memory, and `ORB_SNAPSHOT_ID` is
   ignored, so no snapshots. On Amp's own matrix the chart is currently ahead
   of jorb.

What jorb has that the chart does not, and therefore what it is for:

- **Pause and resume are local.** The chart round-trips an orb's files to a
  storage bucket; jorb leaves a reflink clone in place and boots it again.
  For a workspace with a build tree that is seconds against minutes, and no
  egress.
- **It runs on hardware you own.** The chart needs a managed cluster.
- VM-level isolation rather than a shared kernel. Worth saying plainly: the
  documentation makes no claim about isolation or security boundaries, so this
  is our inference and not Amp's.

Decisions following from that:

- **Scale-out is scoped to owned hardware**, yours or a coworker's. AWS is out:
  KVM there means bare metal instances, and paying for `c7g.metal` to avoid a
  free Helm chart is backwards. A dedicated box is the cheap capacity answer.
- **Multi-host needs placement, not packaging.** An orb's disk is on one host
  and Amp calls one provider command, so something has to route a job to the
  host holding that orb. The simple version is `hosts[hash(orb_id) % n]`:
  stateless, deterministic, no record to keep, and it breaks only when the
  host set changes. Not built yet.
- **Host setup belongs in the binary, not in Nix**, so that NixOS and a
  coworker's Debian run the same code. `jorb preflight` exists as of today;
  `jorb init` is next.
- **The base image should be built in CI, not on each host.** It does not need
  to be fresh: A7's correction records that `/usr/local/bin/amp` is a symlink
  to a binary Amp replaces on startup, so a stale base image costs a larger
  self-update on an orb's first boot and nothing else. The Amp image is 1531
  MiB compressed over 34 layers, so about 4 GiB unpacked. Ship the rootfs as a
  zstd tarball per architecture and let `jorb init` do `tar -x` plus
  `mkfs.ext4 -d`: that needs only root, tar and e2fsprogs, where the current
  path needs skopeo, umoci and jq, and umoci is not packaged on Debian. It
  also takes the network out of the install path, which matters because
  `jorb-prepare-image` is an `ExecStartPre` of the provider today, so a host
  with no network wedges it at boot.

## `jorb preflight`

Every question the gates used to answer by hand, as a command, so a host that
is not the machine jorb was written on can be checked: `/dev/kvm` opens for
reading and writing, cgroup v2, systemd answers, the VMM runs, the guest
kernel is there and not empty, the state and run directories exist, the state
directory can actually clone, the base image is there, there is memory for
every slot at once, and how many slots are free. One line each, non-zero exit
if any failed.

`tests/host/g3-reflink.sh` is deleted: the clone probe replaces it and is
better, because it tells a filesystem that cannot clone from a directory the
caller may not write to. G3 reported `findmnt` output and an 8 MiB `cp`; this
reports the filesystem's name either way, and it refuses to blame XFS for a
permission error. The first run of it found exactly that: unprivileged against
the real `/var/lib/jorb`, the honest answer is "could not be written to", not
"needs XFS".

## `jorb init`

The other half of preflight: preflight asks, init changes the host and then
asks, so what it leaves behind is described by the same checks. Every step is
idempotent.

```
jorb init [--create-state-image] [--state-image <path>] [--state-image-gib <n>]
```

It refuses to run as anyone but root, creates the state, disks and run
directories at mode 0700, gives the state directory a filesystem that can
clone if it has not got one, builds the base image if it is missing, and ends
by running preflight.

**Creating the filesystem is opt-in.** Mounting something is a persistent
change to a machine, and jorb is meant to land on coworkers' hosts, so without
`--create-state-image` it prints what it would have done and carries on:

```
--   /tmp/x/state cannot clone, so every spawn would copy 64 GiB.
     Pass --create-state-image to put a 128 GiB XFS image at /var/lib/jorb.img
     and mount it there, or mount a filesystem that can clone on /tmp/x/state
     yourself.
```

The image is sparse: a 2 GiB one measured 2.0G apparent and 65M on disk. The
mount it makes lasts until the host reboots, and it says so, with the fstab
line, because the Nix module is what should own the mount on NixOS.

Two things fell out of writing it:

- **Commands print, jobs log.** A job explains itself to the dashboards, so it
  logs at DEBUG; a command explains itself to whoever ran it, so it reports on
  stdout and logs only what went wrong. The level is chosen from the mode. The
  first version logged `--help` as an ERROR event with the whole help text in
  one JSON field, which is as bad as it sounds.
- **`preflight` and the VMM check now go through `host::run`**, so there is one
  way jorb runs a program rather than two. That made preflight async, which it
  may as well be.

## The module makes the host ready itself

`jorb init` is a unit now, so importing the module is the whole installation.

- **`jorb-setup.service`**, a oneshot before the provider, runs
  `jorb init --create-state-image …`. The provider `Requires` it, so a host
  that is not ready has no provider taking jobs it cannot finish. The flake's
  module check greps the generated units for both facts.
- **`stateImage` and `stateImageGiB`** are options. Setting `stateImage` makes
  init create and mount XFS on the state directory, which is what lets the
  module work on a host whose filesystem cannot clone. Leave it null where the
  state directory is already XFS or btrfs.
- **The base image stopped being a hidden `ExecStartPre`** of the provider. It
  is part of setup, so a slow pull looks like what it is instead of a provider
  that will not start, and a host with no network fails on the unit that needed
  the network.
- **tmpfiles no longer creates the directories.** `jorb init` owns them, so
  there is one owner and the same code runs on a host without Nix.
- **`JORB_*` is defined once**, as `hostEnvironment`, and used by both things
  that run jorb as root: the sudo rule's `jorb-root` and the setup unit. They
  cannot disagree about which host this is.
- **`jorb-root` still takes no arguments and passes none on.** jorb's commands
  create filesystems and pull images; the provider is closed-source and
  unprivileged, so the sudo rule reaches one job and nothing else. The
  sudoers line is unchanged.

Two assertions, because both failures are quiet ones: a `stateImage` inside
`stateDirectory` would be hidden by the mount over it, and more than 256 slots
cannot be addressed. Writing the first one caught a bug in itself:
`lib.hasPrefix "/var/lib/jorb" "/var/lib/jorb.img"` is true as a string, so
the obvious form would have rejected the obvious configuration. It compares
against `"${stateDirectory}/"`, and the module check now evaluates a host with
`stateImage = "/var/lib/jorb.img"` so that stays true.

## Naming and shape, on the owner's reading, 2026-11-18

- **`fixed.rs` is `constants.rs`.** It holds constants. The previous name was
  mine, twice over: `limits.rs` held things that bounded nothing, and `fixed`
  was no better.
- **`HostConfig` holds the `JORB_*` half of an invocation**, which is the
  trusted half, and the host-wide paths those directories imply: the base
  image, the lock, the disks directory, the partial disk. That is its whole
  role. Three of its methods were per-orb — `disk(orb)`, `config_disk(orb)`,
  `api_socket(orb)` — and every caller was `orb.rs` passing `self.id`, so they
  are `Orb`'s own now: `orb.disk()`, `orb.config_disk()`, `orb.api_socket()`.
  An orb owning its files is why `HostConfig` looked like it had accessors for
  things that were not its own.
- **`Host` is `HostConfig`**, and it no longer allocates anything.
 It is this
  machine's description: sizing, and where the files live. `lowest_free_slot`
  moved to `Slot::lowest_free(host)`, next to `Slot::is_free` and
  `Slot::all(host)`, which is where slot allocation belongs. `Slot::all` also
  removed the slot-iterating loop that had been written twice.
- **Names that said nothing** now say what they do to what: `converge` is
  `run_job`, `take_lock` is `take_host_lock`, `recover` is `stop_dead_orbs`.
  The last one is `recover` in AGENTS.md's model and its doc comment says so,
  so the vocabulary survives where the call site reads plainly.
- **The modules say how the pieces fit**, so no comment has to. `request.rs`
  is what Amp asked for, `host.rs` is this machine and its slots, `systemd.rs`
  owns an orb unit's lifetime, `orb.rs` composes those into the four
  primitives, and `preflight.rs` and `init.rs` are what an installer runs. The
  unit naming, the `systemd-run` invocation and the stop-and-wait left `orb.rs`
  and `host.rs` for `systemd.rs`, which is the only module that knows systemd
  exists.
- **init's functions are verbs**: `create_directories`,
  `create_state_filesystem`, `create_image`, `build_base_image`, and then
  preflight, so creating and asserting are told apart by the names.
- **init's root check is `preflight::root()`**, which returns the evidence or
  the error that says how to become root, used by `?` with context rather than
  by `matches!` and `bail!`. One question, two callers.
- **preflight has no reporting type.** Every question returns
  `Result<String>`: the evidence, or the error that is the answer. That
  deleted `Check`, `passed`, `failed`, a hand-formatted message per failure,
  and a `writeln!` into a `String` that had needed a banned `expect`. `ensure!`
  and `with_context` do the work now.

Argued and kept: **`ConfigDisk`**. AGENTS.md's Fixed interfaces defines "config
disk" and tells us to use the project's vocabulary rather than coin synonyms;
jorb-init calls it that from the guest side; and it is literally a disk
carrying configuration. `GuestConfig` hides that it is a block device and
`TokenDisk` is narrower than its contents. Renaming it is a Fixed interfaces
change, so it waits on the owner.

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
5. Removed by S2: jorb supports aarch64 NixOS only.

## Proposals

1. **Orb units are now `jorb-orb-<id>.service`.** Accepted by the owner on
   2026-11-18 and done, so this is a record rather than a proposal. The old
   `jorb-<id>` pattern caught two other kinds of unit: `jorb-provider.service`,
   which parsed as an orb called "provider", and the gates' own
   `jorb-gate-<id>` units, which run VMs in `jorb.slice` and whose ids parse
   cleanly, so recover could have stopped a gate mid-run. Both are out of reach
   by name now, and recover no longer asks systemd for each unit's slice to
   tell them apart.
2. **A second architecture.** Added at the owner's request and removed again
   by S2, when the owner scoped jorb to aarch64 NixOS.

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
- The fragments are `nix/guest-kernel.config` plus
  `nix/guest-kernel-arm64.config`.
  `nix/kernel-config-check.sh` fails the build if the final `.config` does not
  honour every line of them, so a symbol that loses its dependencies cannot
  slip through to a guest that will not boot.
- A second architecture was built out here and removed by S2.

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

Results live in Gate results above, which is the only copy: this section had a
second table of its own and the two disagreed as soon as the gates were
actually run.

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
  aarch64. The arm64 digest at the time of
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

Files renamed, because the names had stopped describing the contents:

- `job.rs` is `request.rs`. Renaming the type to `Request` and leaving the
  module called `job` was half a job.
- `limits.rs` is `constants.rs` in `jorb` and `jorb-init`. It had drifted into
  holding everything constant, and `SYSTEMCTL`, `GUEST_HOME` and `STATE_DIR`
  bound nothing. The name now covers what is actually in there: the bounds
  plus the names, paths, devices and addresses the interface pins down.
  AGENTS.md's Bounds rule was reworded to match.
- `jorb-vmm/src/limits.rs` is gone. It held two constants, which are at the
  top of `lib.rs` now, because a module for two is more structure than it
  saves.

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

## External review of f95635c, 2026-11-18

An external review of `f95635c` listed findings by phase (P0, S1-S3, C1-C3,
R1-R9, B1-B5, N1-N5, D1-D3, V1-V5). Each was checked against the code before
being acted on. One commit per finding, named by its ID; this section records
what was checked and what each check printed. The owner decided the four
`[owner]` items on 2026-11-18: R7 replaces the builder with a shape struct, B3
powers off when bootstrap exits, N3 uses `mkDefault` plus assertions, and the
working-tree deletion of `PLAN.md`, `PROGRESS.md` and `tests/host/` was
reverted.

### How the checks ran

This session ran on an aarch64-darwin laptop, not on the jorb host, and nix
here has no Linux builder. So:

- `nix flake check` on this machine evaluates the flake and reports "all
  checks passed", but it omits `aarch64-linux`, so it builds none of the
  checks. That pass is not evidence and is not recorded as one.
- fmt, clippy and the test suite ran in an aarch64 Linux container
  (`rust:latest`, cargo 1.100.0, the same version as the flake's toolchain) with the workspace mounted and
  `CH_OPENAPI_SPEC` pointed at the spec from the same `cloud-hypervisor.src`
  the flake uses. That is called "the container check" below.
- Nix evaluation of the module (`nix eval` of the check's `drvPath`, which
  forces the NixOS configuration) runs on this machine without building.
- Every `[host]` check is recorded as awaiting a host run.

### P0. Baseline

- The container check at `f95635c`: `cargo fmt --all --check` clean,
  `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo test
  --workspace` 27 passed (20 in `jorb`, 5 in `jorb-init`, 2 in `jorb-vmm`).
- `nix flake check`: not run on Linux this session (see above).
- `[host]` matrix sections 1, 2, 3 and 6: awaiting a host run. The review asks
  for these before and after phase 3; neither run happened in this session,
  so phase 3 went ahead on unit tests and evaluation alone.

### S1. Remove the installer from the job binary

Confirmed: `jorb/src/main.rs` chose between a job and the `preflight` and
`init` commands, with two logging levels and two `allow(clippy::print_stdout)`
attributes. Deleted `init.rs`, `preflight.rs`, the dispatch and the fourteen
constants only they used. `jorb` now refuses any argument.

- `jorb anything` in the container: one event,
  `{"level":"ERROR","fields":{"message":"jorb takes no arguments: the job is in the environment","outcome":"rejected","argument":"anything"}}`,
  exit 1.
- `grep -rn 'println\|print_stdout' jorb/src`: nothing.
- The container check: fmt and clippy clean, 27 tests pass. Neither deleted
  module had tests.
- AGENTS.md's Contract bullet about commands is rewritten to match.

### S2. Remove x86_64

Confirmed at every place the review named, plus two more in `tests/host`
(`lib.sh`'s console lookup and `g6-amp-image.sh`'s image choice). Removed all
of them; the kernel derivation now builds `arch/arm64/boot/Image` only.

- `git grep -i x86 -- ':!*.lock'`: only this section of PROGRESS.md, which
  has to name what it removed. Read literally, `grep -ri x86 .` also searches
  `target/` and `.git`, which hold build products and history, so the
  tracked-files form is the check that means something.
- `nix eval` of `packages.aarch64-linux.{guest-kernel,jorb-prepare-image}.drvPath`:
  both evaluate. Building them needs the host.
- `nixfmt --check flake.nix nix/*.nix` and `shellcheck -x` over every script:
  clean. The container check: clean, 27 tests pass.
- `nix flake check`: not run on Linux.

### S3. Host readiness moves into the Nix module

Confirmed: `jorb-setup.service` ran `jorb init` (now deleted by S1), and the
`stateImage` assertion was `!lib.hasPrefix "/" cfg.stateImage`, which rejects
every absolute path. PROGRESS.md's earlier claim that it compared against
`"${stateDirectory}/"` was wrong; see D1.

Now, in `nix/module.nix`:

- tmpfiles creates the state directory, its `disks` and the run directory at
  0700. jorb no longer creates them itself, so they have one owner; a missing
  directory is a loud error in the job.
- With `stateImage` set, `jorb-state-image.service` (no default dependencies,
  `RequiresMountsFor` the image's parent) creates the file only if absent:
  truncate and `mkfs.xfs` under `<image>.partial`, then rename. A
  `fileSystems` loop mount with `nofail`, `discard`,
  `x-systemd.requires=jorb-state-image.service`, and
  `x-systemd.before=systemd-tmpfiles-setup.service` so the directories are
  made on the mount rather than under it.
- `jorb-base-image.service` runs `jorb-prepare-image`: `TimeoutStartSec=30min`,
  `Restart=on-failure` every 30 s, no start limit. The provider `Requires` and
  orders after it, and `RequiresMountsFor` the state directory.
- The assertion compares against `"${cfg.stateDirectory}/"`.

Checks:

- `nix eval` of the check host's `system.build.toplevel.drvPath`, which runs
  the NixOS assertions: evaluates. With `stateImage = "/var/lib/jorb/inside.img"`
  it fails with the assertion's message, as it should.
- The generated fstab line:
  `/var/lib/jorb.img /var/lib/jorb xfs loop,nofail,discard,x-systemd.requires=jorb-state-image.service,x-systemd.before=systemd-tmpfiles-setup.service 0 2`.
- The flake's module check greps for `Requires=jorb-base-image.service` and
  that fstab line instead of the old setup unit. Its `drvPath` evaluates; the
  check itself needs a Linux builder.
- nixfmt clean; the container check clean.
- `[host]` boot with no base image ends with the provider active and the
  image built: awaiting a host run. One thing that run should watch: whether
  the provider's start job keeps waiting while `jorb-base-image` is in its
  restart delay. systemd is meant to hold a dependent job across an automatic
  restart, but this has not been observed here.

### C1. The sudo rule gave the provider user root

Confirmed: the rule carried `SETENV`, `jorb-root` never set `JORB_CH_BIN`,
and that variable chose the program `systemd-run` started as root. With
`SETENV`, `sudo FOO=1 jorb-root` passes `FOO` past `env_reset`.

- `SETENV` is gone and the rule has `runAs = "root"`. Generated line:
  `jorb ALL=(root) NOPASSWD: /nix/store/…-jorb-root/bin/jorb-root`.
- `jorb-root` unsets every exported variable except the four `ORB_*`/`AMP_URL`
  names, then exports `PATH` (systemd only), `JORB_CH_BIN` (the VMM as an
  absolute store path) and every other `JORB_*` setting. The review said
  `exec env -i` with an explicit list; that would put `ORB_TOKEN` in `env`'s
  argv, which AGENTS.md forbids, so the shell unsets instead. Same result.
- The module check now greps the sudoers for that line and fails on any
  `SETENV`. Its `drvPath` evaluates; building it needs Linux.
- The cleaning loop, run alone under `env -i FOO=1 LD_PRELOAD=x
  JORB_CH_BIN=/evil ORB_ID=a ORB_TOKEN=t HOME=/root bash`, left only
  `ORB_ID`, `ORB_TOKEN`, the exported settings and bash's own `SHLVL`.
- `[host]` `sudo -u jorb sudo -n FOO=1 <jorb-root>` is refused: awaiting a
  host run.

### C2. The dispatch ran a sudo that is not setuid

Confirmed: `jorb-dispatch` had `pkgs.sudo` first on its `PATH`, and the
provider unit's `path` held it too. The store copy has no setuid bit.

- `jorb-dispatch` runs `${config.security.wrapperDir}/sudo -n …` by absolute
  path; `pkgs.sudo` is gone from its `runtimeInputs` and from the unit's
  `path`. The evaluated provider unit's `Environment=PATH` no longer names a
  sudo package.
- `[host]` `sudo -u jorb env ORB_OPERATION=pause ORB_ID=review-c2 <jorb-dispatch>`
  exits 0: awaiting a host run.

### C3. The VMM could differ from the one the client was generated for

Confirmed: `cloudHypervisorPackage` defaulted to the host's
`pkgs.cloud-hypervisor`, while `CH_OPENAPI_SPEC` came from the flake's own
nixpkgs.

- The flake exports `packages.<system>.cloud-hypervisor` and its module sets
  `cloudHypervisorPackage = mkDefault` that package, as it does for `package`
  and `kernel`.
- The module check asserts, at evaluation, that the module's VMM `src` is the
  `src` the spec comes from. Its `drvPath` evaluates.
- Evaluated on the check host: the module's VMM is
  `/nix/store/sii8k9vh…-cloud-hypervisor-54.1`, the same path as the flake's
  package, and its `src` is `/nix/store/475y2r9n…-source`, which is where
  `CH_OPENAPI_SPEC` points.

### R3. Remove clap

Confirmed: the `Only` parser shim was in three test modules, and `Request`
had a `--token` flag that must never be used.

Phase 3 was done in the order R3, R2, R6, R7, then R1, R4 and R5 together,
then R8 and R9, because R1, R4 and R5 are one move of the same code and
splitting them would mean writing each file three times.

- `Request::from_environment` and `HostConfig::from_environment` read through
  an `Environment` (a `&dyn Fn(&str) -> Result<Option<String>>`), which is
  `std::env::var` in a job and a table in a test. A set but non-UTF-8 variable
  is an error, not absent. Edition 2024 makes `set_var` unsafe, which is why
  tests pass a table rather than set the process environment.
- Behaviour change: `JORB_SLOTS` outside `1..=256` is refused at parse time.
  clap accepted any `NonZeroU16` and the code clamped it to 256.
- `clap` is gone from `jorb/Cargo.toml` and `Cargo.lock`
  (`grep -c '"clap' Cargo.lock` is 0); no `struct Only` remains. AGENTS.md's
  allowlist is updated.
- The container check: clean, 28 tests (one new, for refused host settings).
- In the container: `jorb` with no variables logs `"error":"ORB_OPERATION is
  not set"`; `ORB_ID=../x` logs `"ORB_ID: an orb id must start with a letter
  or a digit, and '.' does not"`; `JORB_SLOTS=300` logs `"JORB_SLOTS is 300,
  and a host has between 1 and 256 slots"`. Each exits 1.

### R2. The job is a type that cannot be wrong

Confirmed: `Request` held an operation and an `Option<SecretString>`, and a
spawn without a token was refused late, inside `ConfigDisk::new`.

- `enum Job { Spawn { orb, config_disk }, Pause { orb }, Delete { orb } }`,
  parsed once by `Job::from_environment`. The token is turned into the config
  disk during parsing and not kept. `ConfigDisk::new` takes `&SecretString`.
- `request.rs` is `job.rs`, so the module and the type have the same name
  (part of R8's "use job").
- Behaviour change: an empty `ORB_TOKEN` is refused. Before, it wrote
  `ORB_TOKEN=` to the config disk, which jorb-init accepts as present.
- The `op` log field is now Amp's spelling (`spawn`), not the enum's Debug
  form (`Spawn`). The dashboard groups by it and does not match on values.
- `grep -rn 'Option<SecretString>' jorb/src`: nothing. Missing and empty
  tokens are parse errors with their own test, `spawn_without_a_token_is_a_parse_error`.
- The container check: clean, 28 tests.

### R6. One definition of the host-guest contract

Confirmed: jorb encoded the config disk in `request.rs` and jorb-init parsed
it in its `main.rs`, each with its own `CONFIG_DISK_BYTES`, and they
disagreed: jorb-init's parser trimmed each line, so a value with leading or
trailing whitespace would not read back as written. jorb-init also used
`str::lines`, which drops a trailing `\r`.

- New crate `jorb-config-disk`, no dependencies: `BYTES`, `ConfigDisk::encode`,
  `parse`, and an `Error` whose messages never carry a value. The parser
  splits on `\n` only and trims nothing. A line without `=` is now an error
  rather than skipped, because the encoder cannot produce one.
- jorb's `job::config_disk(token, amp_url)` builds the entries; jorb-init
  calls `jorb_config_disk::parse`. Both constants files lost their copy of
  the size. PLAN.md's T5 chose two parsers over a shared crate; this reverses
  that, as the review asked, because the two had already drifted.
- The round-trip test encodes and parses every string of up to three
  characters over `a`, space, `=`, `\r`, tab, `#`, `é` and an emoji (585
  values) and requires byte-identical results. The shape, refusal, padding
  and redaction tests moved into the crate.
- AGENTS.md's Crates table has the new crate, and both binaries' allowlists
  name it.
- The container check: clean, 30 tests (18 `jorb`, 6 `jorb-config-disk`,
  4 `jorb-init`, 2 `jorb-vmm`).

### R7. Narrow jorb-vmm to what is used

Confirmed: `boot` returned a `Vm` every caller dropped; `huge_pages` set
both `hugepages` and `thp` and defaulted to true, so a builder without
`without_huge_pages()` asked for hugetlbfs; and the crate had a `Vmm`
handle, where AGENTS.md says an API socket is the only name for a VM.

- The owner chose (2026-11-18) to replace the builder. `VmShape` is a struct
  of exactly the fixed fields: kernel, cmdline, cpus, memory, a writable root
  disk, a read-only disk and a `Tap`, all required. Huge pages are always
  off and free page reporting always on; neither is a field.
- The crate's interface is `boot(socket, &shape) -> Result<()>` and
  `state(socket) -> Result<VmState>`. `Vmm` and `Vm` are gone; each call
  builds its client from the socket. That is what AGENTS.md and PLAN.md T4
  described all along (see D1).
- `git diff jorb-vmm/tests/vm-shape.json`: empty. The golden JSON is
  unchanged.
- The boot example and AGENTS.md's Crates row follow. PLAN.md T4 still shows
  the builder; it is the plan as written and is left as history.
- The container check: clean, 30 tests.

### R1, R4, R5. Organise jorb by the model

Confirmed: `host.rs` held settings, paths, `Slot` and the subprocess runner;
`Orb` was two borrows used as a namespace; the operations were split across
`main.rs` match arms, a free function and `orb.rs` methods; recover was named
`stop_dead_orbs`; `main.rs` mentioned a `converge` that no longer existed; the
VM shape was assembled from `orb.rs`, `host.rs`, `systemd.rs` (the VMM flag)
and `constants.rs`; and `constants.rs` held 51 constants by kind. Done as one
commit, because the three findings are one move of the same code.

| File | Question |
|---|---|
| `main.rs` | Parse, lock, run, report |
| `job.rs` | What did Amp ask? `Job`, `OrbId`, the spawn's config disk |
| `settings.rs` | What is this host configured as? `Settings`, the `JORB_*` defaults |
| `operations.rs` | `recover`, `spawn`, `pause`, `delete`, and `stop`, `ensure_disk`, `remove_disk`, `start` |
| `unit.rs` | systemd: unit names, list, start, stop; and `run`, the one way jorb runs a program |
| `disk.rs` | The state- and run-directory layout, and the config disk file |
| `vm.rs` | `Slot`, the cmdline, the VMM's command line, `state`, `boot` |
| `limits.rs` | Durations, counts and sizes only |

- `operations.rs` opens with AGENTS.md's operation table, and the four
  operations below it are that table in code. No type named `Orb`; the
  function is `recover`.
- `run` sits in `unit.rs` rather than a module of its own because systemd's
  tools are all it runs in this phase. N1 adds `nft`, at which point that
  placement is revisited.
- The VMM's command line (`cloud-hypervisor --api-socket=…`) moved from the
  systemd module to `vm::vmm_command`; `unit::start` takes a command and
  knows nothing about what it is. Changing the cmdline touches
  `vm.rs` and its golden-string test only. A disk change also touches
  `jorb-vmm`'s `VmShape` and golden JSON, since R7 made every disk a field.
- `limits.rs` holds 15 bounds and sizes; no path, program or device. The
  names moved to the module that uses them (for example `UNIT_PREFIX` and
  `SYSTEMCTL` in `unit.rs`, `GUEST_NETMASK` and `CONSOLE_DEVICE` in `vm.rs`).
  AGENTS.md's Bounds rule now says that.
- Path functions are `…_path` and the settings are `state_directory` and
  `run_directory` (R8's Rust half).
- Behaviour is unchanged: the same calls in the same order. The container
  check: clean, 29 tests. One `jorb` test went: "removing a missing path is
  not a failure" is now the last line of the config disk test.
- `jorb` is 1370 lines including tests and comments, against 1483 before (`wc -l` over `jorb/src`).

### R8. Names

Confirmed for each item.

- Path functions: done in R1 (`lock_path`, `disk_path`, `base_image_path`,
  `config_disk_path`, `partial_disk_path`, `api_socket_path`).
- `state_dir` beside `state_directory()`: done in R1; `Settings` has
  `state_directory` and `run_directory`.
- invocation, request, job, command: the input is a `Job` in `job.rs` (R2);
  `Invocation` and `run_command` are gone (S1, R1). `git grep -n -i
  'invocation\|request' -- 'jorb/src'` finds nothing.
- Test scripts: `g4-boot.sh` is `boot-under-systemd.sh`,
  `g5-free-page-reporting.sh` is `free-page-reporting.sh`, `g6-amp-image.sh`
  is `amp-image.sh`, `g7-observability.sh` is `observability.sh`,
  `t5-jorb-init.sh` is `jorb-init-in-guest.sh`. Each still opens with the gate
  or task it answers, because the results above are recorded by that ID.
- `nix/test-image.nix`, `nix/test-init.sh` and `nix/test-amp.sh` are
  `tests/image/image.nix`, `init.sh` and `amp.sh`. The flake and "What to run
  next" follow.
- `shellcheck -x` over every script and `nixfmt --check` over every Nix file:
  clean. `packages.aarch64-linux.test-image` evaluates.

### R9. Comments

Confirmed: comments narrated history ("used to be") and recorded gates (`A2,
verified 2026-11-18`), and `nix/image.nix` said the script runs as the
provider's `ExecStartPre`.

- The `jorb` modules were rewritten in R1 with one `//!` paragraph each and
  comments only for what the code cannot say. The jorb-vmm gate records went
  in R7, the jorb-init `A2` record and a lib.sh paragraph about a fixed bug
  here. `nix/image.nix` now names `jorb-base-image.service`.
- `git grep -i -E 'used to|no longer|verified|ExecStartPre|20[0-9]{2}-[0-9]{2}-[0-9]{2}'`
  outside PROGRESS.md and PLAN.md: only AGENTS.md's instruction text and
  test scripts' "a previous run" wording, which describe what the code
  guards against rather than history.
- Comment lines against code lines (non-blank, tests included) in the Rust
  is now 382 to 1640, about one to four; it was about one to three.
  `ASSUMES A11` in `nix/jorb.alloy` stays: A11 is not yet verified, and the
  marker is what AGENTS.md asks for.
- shellcheck and the container check: clean.
