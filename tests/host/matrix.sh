#!/usr/bin/env bash
# T11: what jorb does, checked against what the plan says it should do.
#
# Ten sections, each printing its own PASS or FAIL line plus evidence, and
# one summary line at the end. Everything runs against the busybox test image
# with a dummy token: no Amp, no real orb, no test hooks in jorb itself.
#
# Usage: sudo -E tests/host/matrix.sh [section...]
#   with no arguments it runs all ten.

# The sections are dispatched by name, which hides every call from here.
# shellcheck disable=SC2329

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

need_root
need_kvm

# setup() removes its whole tree, so being pointed at the production state
# directory would delete the real base image.
if [ "${JORB_MATRIX_DIR:-}" = /var/lib/jorb ]; then
	fail "JORB_MATRIX_DIR is the production state directory, which the matrix would wipe"
fi

# Smaller than a real orb so a section does not wait on memory it will not
# use. One slot, because the recovery section is about slot contention.
slots=${JORB_SLOTS:-1}
export JORB_SLOTS
JORB_SLOTS=$slots
export JORB_VCPUS=${JORB_VCPUS:-2}
export JORB_MEMORY_MIB=${JORB_MEMORY_MIB:-2048}

# Under the XFS mount, not in /var/tmp: every disk jorb makes is a reflink
# clone of the base image, and /var/tmp is ext4 on this host, where
# ioctl_ficlone fails with EOPNOTSUPP. The only symptom of that is "spawn
# exited non-zero", so setup probes for it below rather than leaving every
# section to discover it.
work=${JORB_MATRIX_DIR:-/var/lib/jorb/matrix}
export JORB_STATE_DIR=$work/state
export JORB_RUN_DIR=/run/jorb-matrix
export JORB_KERNEL

token=matrix-dummy-token

# Section 4 listens on this port on every host address, and the guest tries
# to connect to it through its tap.
probe_tcp_port=${JORB_MATRIX_PROBE_TCP_PORT:-47123}
failures=0
sections_run=0

# How long to wait for an orb to come all the way up, and for a spawn that
# cannot work to give up. The second one is API_SOCKET_WAIT plus room for the
# unit to start at all.
boot_deadline=${JORB_MATRIX_BOOT_DEADLINE:-90}
no_socket_deadline=${JORB_MATRIX_NO_SOCKET_DEADLINE:-40}
# jorb's LOCK_WAIT plus JOB_DEADLINE, the longest a job can run.
job_deadline=280

# ---------------------------------------------------------------- the harness

jorb=${JORB_BIN:-}
section_result=

setup() {
	# The only full wipe: everything below keeps the base image.
	rm -rf "$work" "$JORB_RUN_DIR"
	mkdir -p "$work" "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	JORB_KERNEL=$(built "${JORB_KERNEL_FLAKE:-.#guest-kernel}")/kernel
	image=$(built "${JORB_TEST_IMAGE_FLAKE:-.#test-image}")/test.raw
	if [ -z "$jorb" ]; then
		jorb=$(built "${JORB_FLAKE:-.#jorb}")/bin/jorb
	fi

	# jorb refuses to start an orb without the network policy's table, so
	# without the module no spawn in any section can succeed.
	if ! module_installed; then
		fail "the inet jorb nftables table is not loaded; install the module first"
	fi

	evidence "jorb     $jorb"
	evidence "kernel   $JORB_KERNEL"
	evidence "image    $image"
	evidence "state    $JORB_STATE_DIR"

	wipe_everything

	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"
	# The matrix's base image is the busybox one, so an orb boots in seconds
	# and needs no Amp.
	install -m 0600 "$image" "$JORB_STATE_DIR/base.raw"
	write_probe_targets "$JORB_STATE_DIR/base.raw"

	# Without reflinks no spawn can work, and the failure arrives as a bare
	# non-zero exit from the first job. Ask here, where the answer can say so.
	if ! cp --reflink=always "$JORB_STATE_DIR/base.raw" \
		"$JORB_STATE_DIR/disks/.reflink-probe" 2>"$work/reflink-error"; then
		evidence "cp said: $(cat "$work/reflink-error")"
		evidence "$(findmnt -T "$JORB_STATE_DIR" --noheadings --output TARGET,FSTYPE,SOURCE)"
		fail "$JORB_STATE_DIR cannot reflink, so every ensure_disk would fail; point JORB_MATRIX_DIR at the XFS mount"
	fi
	rm -f "$JORB_STATE_DIR/disks/.reflink-probe"
}

# The addresses section 4 cannot work out from inside the guest, written into
# the matrix's own copy of the base image.
write_probe_targets() {
	local base=$1 default_route host_lan neighbour
	default_route=$(ip -o -4 route show default | head -n 1)
	host_lan=$(printf '%s' "$default_route" | grep -o 'src [0-9.]*' | cut -d' ' -f2)
	neighbour=$(printf '%s' "$default_route" | grep -o 'via [0-9.]*' | cut -d' ' -f2)

	printf 'JORB_PROBE_HOST_LAN=%s\nJORB_PROBE_LAN_NEIGHBOUR=%s\nJORB_PROBE_HOST_TCP_PORT=%s\n' \
		"$host_lan" "$neighbour" "$probe_tcp_port" >"$work/probe-targets"

	debugfs -w -R "rm /etc/jorb-probe-targets" "$base" >/dev/null 2>&1 || true
	debugfs -w -R "write $work/probe-targets /etc/jorb-probe-targets" "$base" >/dev/null 2>&1

	evidence "probes   host $host_lan, neighbour $neighbour"
}

# Runs one job the way the provider does: environment in, exit code out.
job() {
	local operation=$1 orb=$2 job_token=${3:-}
	local status=0

	if [ -n "$job_token" ]; then
		ORB_OPERATION=$operation ORB_ID=$orb ORB_TOKEN=$job_token \
			timeout 300 "$jorb" 2>>"$work/jorb.log" || status=$?
	else
		ORB_OPERATION=$operation ORB_ID=$orb \
			timeout 300 "$jorb" 2>>"$work/jorb.log" || status=$?
	fi

	return "$status"
}

# The unit an orb runs as, and the units of every orb on the host. The
# jorb-orb- prefix is what keeps the provider's unit and the gates' own
# jorb-gate- units out of both answers.
unit_of() {
	printf 'jorb-orb-%s.service' "$1"
}

orb_units() {
	systemctl list-units --all --plain --no-legend --no-pager 'jorb-orb-*.service' |
		awk '{print $1}'
}

# Everything the host knows about an orb, as three words: unit, vm, disk.
observed() {
	local orb=$1 unit vm disk

	if systemctl is-active --quiet "$(unit_of "$orb")"; then
		unit=active
	else
		unit=inactive
	fi

	if [ -S "$JORB_RUN_DIR/$orb.sock" ]; then
		vm=$(api "$JORB_RUN_DIR/$orb.sock" GET vm.info 2>/dev/null | jq -r '.state' 2>/dev/null)
		vm=${vm:-unreachable}
	else
		vm=no-socket
	fi

	if [ -f "$JORB_STATE_DIR/disks/$orb.raw" ]; then
		disk=present
	else
		disk=absent
	fi

	printf '%s %s %s' "$unit" "$vm" "$disk"
}

expect_state() {
	local orb=$1 wanted=$2 actual
	actual=$(observed "$orb")

	if [ "$actual" != "$wanted" ]; then
		note "$orb is '$actual', expected '$wanted'"
		return 1
	fi
}

# The debris checks every section ends with: nothing left that a later job
# would trip over.
expect_no_debris() {
	local allowed=${1:-}
	local stray=0

	local units
	units=$(orb_units)
	for unit in $units; do
		local orb=${unit#jorb-orb-}
		orb=${orb%.service}
		if [ "$orb" != "$allowed" ]; then
			note "a unit is left behind: $unit"
			stray=1
		fi
	done

	for tap in /sys/class/net/jorb[0-9]*; do
		[ -e "$tap" ] || continue
		if [ -z "$allowed" ]; then
			note "a tap is left behind: $(basename "$tap")"
			stray=1
		fi
	done

	for socket in "$JORB_RUN_DIR"/*.sock; do
		[ -e "$socket" ] || continue
		local orb
		orb=$(basename "$socket" .sock)
		if [ "$orb" != "$allowed" ]; then
			note "a socket is left behind: $socket"
			stray=1
		fi
	done

	if [ -e "$JORB_STATE_DIR/disks/.tmp" ]; then
		note "a partial disk is left behind: $JORB_STATE_DIR/disks/.tmp"
		stray=1
	fi

	return "$stray"
}

# Waits for an orb to be all the way up, which is what a spawn promises.
wait_until_running() {
	local orb=$1 waited=0
	while [ "$waited" -lt "$boot_deadline" ]; do
		if [ "$(observed "$orb")" = "active Running present" ]; then
			return 0
		fi
		sleep 1
		waited=$((waited + 1))
	done
	note "$orb never reached 'active Running present': $(observed "$orb")"
	return 1
}

# Waits for the guest to say it is ready, which means jorb-init got all the
# way to bootstrap.
# Waits for one of the stand-in's markers in an orb's journal.
wait_for_guest_marker() {
	local orb=$1 wanted=$2 waited=0
	while [ "$waited" -lt "$boot_deadline" ]; do
		if guest_log "$orb" | grep -q "JORB-GATE $wanted"; then
			return 0
		fi
		sleep 1
		waited=$((waited + 1))
	done
	note "$orb never logged '$wanted'"
	return 1
}

wait_until_ready() {
	wait_for_guest_marker "$1" 'amp ready'
}

# How long an orb took from the spawn job starting to its bootstrap saying it
# was ready, and when the guest kernel reached init by its own clock. The
# second number is what tells a slow kernel from a slow host.
record_latency() {
	local orb=$1 started=$2 log
	log=$(guest_log "$orb")
	evidence "spawn to bootstrap ready: $(($(date +%s) - started))s"
	evidence "kernel reached init: $(printf '%s\n' "$log" | grep -m1 'Run /jorb-init as init process' || echo 'not in the journal')"
	evidence "kernel's last line before init: $(printf '%s\n' "$log" | grep -B1 -m1 'Run /jorb-init as init process' | head -n 1)"
}

# Whether the host carries the jorb module's own configuration. Sections 4 and
# 5 test what that configuration does, so without it there is no policy to
# hold and nowhere for MemoryHigh to reclaim to, and saying so is better than
# reporting it as jorb's failure.
module_installed() {
	nft list table inet jorb >/dev/null 2>&1
}

# This run's journal for an orb, not a previous one's.
#
# Orb names repeat between runs and journald keeps everything, so filtering by
# unit alone reads back whatever an earlier run left: a section would find its
# own marker before the orb it is waiting for had even booted. An empty answer
# means the unit is not running, which is better than a stale one.
guest_log() {
	local invocation
	invocation=$(systemctl show -p InvocationID --value "$(unit_of "$1")" 2>/dev/null || true)
	[ -n "$invocation" ] || return 0
	journalctl "_SYSTEMD_INVOCATION_ID=$invocation" --no-pager --output cat 2>/dev/null || true
}

# Back to nothing: no units, no taps, no sockets, no orb disks. The base
# image and this run's log stay, because every section needs them.
wipe_everything() {
	local units
	units=$(orb_units)
	for unit in $units; do
		systemctl stop "$unit" >/dev/null 2>&1 || true
	done
	for tap in /sys/class/net/jorb[0-9]*; do
		[ -e "$tap" ] || continue
		ip link del "$(basename "$tap")" >/dev/null 2>&1 || true
	done

	rm -rf "$JORB_RUN_DIR"
	rm -f "$JORB_STATE_DIR"/disks/*.raw "$JORB_STATE_DIR/disks/.tmp"
	mkdir -p "$JORB_RUN_DIR" "$JORB_STATE_DIR/disks"
}

# Section bookkeeping. A section records notes as it goes and ends with one
# line of its own.
note() {
	printf '      %s\n' "$*"
	section_result=failed
}

section() {
	local number=$1 name=$2
	printf '\n--- %s. %s\n' "$number" "$name"
	section_result=
	sections_run=$((sections_run + 1))
}

finish_section() {
	local number=$1 name=$2
	if [ -n "$section_result" ]; then
		printf 'FAIL matrix %s: %s\n' "$number" "$name"
		failures=$((failures + 1))
	else
		printf 'PASS matrix %s: %s\n' "$number" "$name"
	fi
}

# The state an operation converges on. Pause keeps whatever disk there was,
# which for an orb that never existed is none: "stopped" and "stopped with a
# disk" are the same target seen from different starts.
wanted_for() {
	local operation=$1 disk=${2:-present}

	case $operation in
	spawn) printf 'active Running present' ;;
	pause) printf 'inactive no-socket %s' "$disk" ;;
	delete) printf 'inactive no-socket absent' ;;
	esac
}

# ------------------------------------------------------- 1. the state matrix

section_1() {
	section 1 "every operation from every start state, twice"

	local orb=m1
	for start in absent stopped running; do
		for operation in spawn pause delete; do
			wipe_everything

			# An orb that never existed has no disk to keep.
			local disk_before=present
			case $start in
			absent) disk_before=absent ;;
			esac

			case $start in
			absent) ;;
			stopped)
				job spawn "$orb" "$token" || note "$start: the setup spawn failed"
				wait_until_running "$orb" || true
				job pause "$orb" || note "$start: the setup pause failed"
				;;
			running)
				job spawn "$orb" "$token" || note "$start: the setup spawn failed"
				wait_until_running "$orb" || true
				;;
			esac

			# Twice, because every operation has to be safe to repeat.
			local attempt
			for attempt in first second; do
				if ! job "$operation" "$orb" "$token"; then
					note "$start/$operation ($attempt) exited non-zero"
					continue
				fi

				if [ "$operation" = spawn ]; then
					wait_until_running "$orb" || true
				fi

				expect_state "$orb" "$(wanted_for "$operation" "$disk_before")" ||
					note "$start/$operation ($attempt) left the wrong state"
			done

			# A running orb is allowed its unit, socket and tap; the others
			# have to leave nothing at all.
			local allowed=
			if [ "$operation" = spawn ]; then
				allowed=$orb
			fi
			expect_no_debris "$allowed" || note "$start/$operation left debris"

			evidence "$start/$operation -> $(observed "$orb")"
		done
	done

	wipe_everything
	finish_section 1 "every operation from every start state, twice"
}

# --------------------------------------------------------------- 2. the kills

section_2() {
	section 2 "fifty killed jobs, then convergence"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	local orb=m2 round
	for round in $(seq 1 "${JORB_MATRIX_KILLS:-50}"); do
		local operation delay
		operation=$(shuf -e spawn pause delete -n 1)
		# Up to 1.5s, so a kill lands anywhere from before the lock to the
		# middle of a boot.
		delay=$(awk -v seed="$round" 'BEGIN { srand(seed); printf "%.2f", rand() * 1.5 }')

		ORB_OPERATION=$operation ORB_ID=$orb ORB_TOKEN=$token \
			timeout -s KILL "$delay" "$jorb" >/dev/null 2>>"$work/jorb-kills.log" || true
	done
	evidence "killed ${JORB_MATRIX_KILLS:-50} jobs at random points"

	# Whatever that left, the next three jobs have to converge from it.
	job spawn "$orb" "$token" || note "spawn after the kill loop failed"
	wait_until_running "$orb" || true
	expect_state "$orb" "$(wanted_for spawn)" || note "spawn did not converge"

	job pause "$orb" || note "pause after the kill loop failed"
	expect_state "$orb" "$(wanted_for pause)" || note "pause did not converge"

	job delete "$orb" || note "delete after the kill loop failed"
	expect_state "$orb" "$(wanted_for delete)" || note "delete did not converge"

	expect_no_debris || note "the kill loop left debris behind"

	wipe_everything
	finish_section 2 "fifty killed jobs, then convergence"
}

# ------------------------------------------------- 3. spawn on a healthy orb

section_3() {
	section 3 "an identical spawn leaves a healthy orb alone"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	local orb=m3
	job spawn "$orb" "$token" || note "the first spawn failed"
	wait_until_running "$orb" || true

	local before after
	before=$(systemctl show -p ActiveEnterTimestampMonotonic --value "$(unit_of "$orb")")
	evidence "booted at $before"

	job spawn "$orb" "$token" || note "the identical spawn failed"
	after=$(systemctl show -p ActiveEnterTimestampMonotonic --value "$(unit_of "$orb")")
	evidence "after an identical spawn: $after"

	if [ "$before" != "$after" ]; then
		note "an identical spawn restarted the VM ($before -> $after)"
	fi

	job spawn "$orb" "$token-changed" || note "the spawn with a new token failed"
	wait_until_running "$orb" || true
	local restarted
	restarted=$(systemctl show -p ActiveEnterTimestampMonotonic --value "$(unit_of "$orb")")
	evidence "after a changed token: $restarted"

	if [ "$restarted" = "$before" ]; then
		note "a changed token did not restart the VM"
	fi

	job delete "$orb" || note "the cleanup delete failed"
	wipe_everything
	finish_section 3 "an identical spawn leaves a healthy orb alone"
}

# ------------------------------------------------------------- 4. the network

section_4() {
	section 4 "an orb reaches the internet and nothing else"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	if ! module_installed; then
		note "no 'inet jorb' nftables table on this host, so there is no policy to test and no NAT to reach Amp through; this section needs the module (T9)"
		finish_section 4 "an orb reaches the internet and nothing else"
		return
	fi

	local orb=m4 listener started
	systemd-socket-activate --listen="0.0.0.0:$probe_tcp_port" --accept cat >/dev/null 2>&1 &
	listener=$!

	started=$(date +%s)
	job spawn "$orb" "$token" || note "the spawn failed"
	wait_until_ready "$orb" || true
	record_latency "$orb" "$started"
	kill "$listener" 2>/dev/null || true

	local probes
	probes=$(guest_log "$orb" | grep 'JORB-GATE amp reach' || true)
	printf '%s\n' "$probes" | sed 's/^/      /'

	# Anything the guest reached that it should not have.
	if printf '%s\n' "$probes" | grep -q 'REACHED'; then
		note "the guest reached something the policy forbids"
	fi

	if ! printf '%s\n' "$probes" | grep -q 'ampcode.com reachable'; then
		note "the guest could not reach ampcode.com, which is the one thing it needs"
	fi

	job delete "$orb" || note "the cleanup delete failed"
	wipe_everything
	finish_section 4 "an orb reaches the internet and nothing else"
}

# -------------------------------------------------------------- 5. the memory

section_5() {
	section 5 "memory the orb frees comes back, and zram absorbs the rest"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	if [ ! -r /sys/block/zram0/mm_stat ]; then
		note "no zram0 on this host, so MemoryHigh has nowhere to reclaim to; this section needs the module (T9), which enables zramSwap"
		finish_section 5 "memory the orb frees comes back, and zram absorbs the rest"
		return
	fi

	local orb=m5
	job spawn "$orb" "workload-memory-$token" || note "the spawn failed"
	# Not 'amp ready': the stand-in runs the workload and only then reports
	# itself ready, so waiting for that would start watching after the
	# gigabyte had already been freed.
	wait_for_guest_marker "$orb" 'amp memory allocating' || true

	local cgroup
	cgroup=$(systemctl show -p ControlGroup --value "$(unit_of "$orb")")

	local peak=0 waited=0
	while [ "$waited" -lt 60 ]; do
		local current
		current=$(cat "/sys/fs/cgroup$cgroup/memory.current" 2>/dev/null || echo 0)
		if [ "$current" -gt "$peak" ]; then
			peak=$current
		fi
		if guest_log "$orb" | grep -q 'JORB-GATE amp memory freed'; then
			break
		fi
		sleep 2
		waited=$((waited + 2))
	done
	evidence "peak while allocating: $(mib "$peak")"

	# Free page reporting is not instant: the balloon reports in batches.
	sleep 30
	local settled
	settled=$(cat "/sys/fs/cgroup$cgroup/memory.current" 2>/dev/null || echo 0)
	evidence "after the orb freed it: $(mib "$settled")"

	if [ "$settled" -ge "$peak" ]; then
		note "the cgroup did not fall after the orb freed a gigabyte"
	fi

	evidence "zram: $(awk '{print "original " $1 " bytes, compressed " $2 " bytes"}' /sys/block/zram0/mm_stat)"

	evidence "slice MemoryHigh: $(cat /sys/fs/cgroup/jorb.slice/memory.high 2>/dev/null || echo unset)"

	job delete "$orb" || note "the cleanup delete failed"
	wipe_everything
	finish_section 5 "memory the orb frees comes back, and zram absorbs the rest"
}

# -------------------------------------------------------------- 6. the bounds

section_6() {
	section 6 "a VMM that never opens its socket fails fast and leaves a unit the next job removes"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	# A program that starts, stays, and never opens an API socket.
	local silent=$work/silent-vmm
	printf '#!/bin/sh\nexec sleep 600\n' >"$silent"
	chmod +x "$silent"

	local orb=m6 started elapsed status=0
	started=$(date +%s)
	JORB_CH_BIN=$silent ORB_OPERATION=spawn ORB_ID=$orb ORB_TOKEN=$token \
		timeout 300 "$jorb" 2>>"$work/jorb.log" || status=$?
	elapsed=$(($(date +%s) - started))

	evidence "spawn exited $status after ${elapsed}s"

	if [ "$status" -eq 0 ]; then
		note "a spawn whose VMM never answered exited 0"
	fi

	if [ "$elapsed" -gt "$no_socket_deadline" ]; then
		note "it took ${elapsed}s to give up, which is past API_SOCKET_WAIT plus slack"
	fi

	# The failed spawn leaves its unit running on purpose. The next job of any
	# orb is what removes it.
	if ! systemctl is-active --quiet "$(unit_of "$orb")"; then
		evidence "the unit was already gone, so there was nothing to recover"
	else
		evidence "the failed spawn left $(unit_of "$orb") behind, as designed"
	fi

	local other=m6b
	job pause "$other" || note "the next job (for another orb) failed"

	if systemctl is-active --quiet "$(unit_of "$orb")"; then
		note "the next job did not remove the leftover unit"
	else
		evidence "the next job removed it"
	fi

	expect_no_debris || note "something else was left behind"

	wipe_everything
	finish_section 6 "a VMM that never opens its socket fails fast and leaves a unit the next job removes"
}

# ------------------------------------------------------------ 7. the recovery

section_7() {
	section 7 "one slot, two orbs, and a VMM killed under it"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	local first=m7a second=m7b

	# A healthy orb in the only slot there is.
	job spawn "$first" "$token" || note "the first spawn failed"
	wait_until_running "$first" || true
	expect_state "$first" "$(wanted_for spawn)" || note "$first did not come up"

	# The power cut: kill the VMM rather than stopping its unit, then spawn
	# again and check the orb comes back on the disk it already had. The
	# socket path is matched on its own, so this does not depend on how jorb
	# spells the argument.
	local disk=$JORB_STATE_DIR/disks/$first.raw
	local inode_before inode_after
	inode_before=$(stat -c %i "$disk")

	pkill -9 -f "$JORB_RUN_DIR/$first.sock" || true
	sleep 2
	evidence "after killing the VMM: $(observed "$first")"

	job spawn "$first" "$token" || note "the spawn after the VMM was killed failed"
	wait_until_running "$first" || true
	expect_state "$first" "$(wanted_for spawn)" || note "$first did not come back"

	inode_after=$(stat -c %i "$disk")
	evidence "disk inode $inode_before -> $inode_after"
	if [ "$inode_before" != "$inode_after" ]; then
		note "the orb came back on a different disk"
	fi

	# A healthy orb is left alone, so with one slot the second orb cannot
	# start. No free slot is the capacity error and there is no other.
	if job spawn "$second" "$token"; then
		note "a spawn of $second succeeded while $first held the only slot"
	else
		evidence "$second was refused while $first held the slot: $(observed "$second")"
	fi

	# Freeing the slot is what lets it start, which is the other half of the
	# same rule.
	job delete "$first" || note "the delete of $first failed"
	job spawn "$second" "$token" || note "the spawn of $second after the slot was freed failed"
	wait_until_running "$second" || true
	expect_state "$second" "$(wanted_for spawn)" || note "$second did not take the freed slot"

	job delete "$second" || note "the cleanup delete failed"
	wipe_everything
	expect_no_debris || note "something was left behind"
	finish_section 7 "one slot, two orbs, and a VMM killed under it"
}

# ------------------------------------------- 8. a VMM that does not answer

section_8() {
	section 8 "a stopped VMM does not block the jobs that clear it"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	local first=m8a second=m8b started elapsed status=0

	job spawn "$first" "$token" || note "the first spawn failed"
	wait_until_running "$first" || true
	expect_state "$first" "$(wanted_for spawn)" || note "$first did not come up"

	# SIGSTOP leaves the VMM alive, holding its socket and slot, and answering
	# nothing.
	pkill -STOP -f "$JORB_RUN_DIR/$first.sock" || note "no VMM to stop"

	started=$(date +%s)
	job delete "$first" || status=$?
	elapsed=$(($(date +%s) - started))
	evidence "delete of a stopped VMM's orb exited $status after ${elapsed}s"

	if [ "$status" -ne 0 ]; then
		note "the delete failed"
	fi
	if [ "$elapsed" -gt "$job_deadline" ]; then
		note "the delete took past the job deadline"
	fi

	job spawn "$second" "$token" || note "the spawn of $second after the delete failed"
	wait_until_running "$second" || true
	expect_state "$second" "$(wanted_for spawn)" || note "$second did not take the freed slot"

	job delete "$second" || note "the cleanup delete failed"
	wipe_everything
	expect_no_debris || note "something was left behind"
	finish_section 8 "a stopped VMM does not block the jobs that clear it"
}

# ------------------------------------------------ 9. bootstrap exiting

section_9() {
	section 9 "an orb whose bootstrap exits powers off and its unit stops"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	local orb=m9 invocation log exits errors waited=0 stopped='' started

	started=$(date +%s)
	job spawn "$orb" "$token-exit-bootstrap" || note "the spawn failed"
	wait_until_ready "$orb" || true
	record_latency "$orb" "$started"
	# Read now: once the unit is collected, guest_log has nothing to go on.
	invocation=$(systemctl show -p InvocationID --value "$(unit_of "$orb")")

	while [ "$waited" -lt "$boot_deadline" ]; do
		if ! systemctl is-active --quiet "$(unit_of "$orb")"; then
			stopped=yes
			break
		fi
		sleep 1
		waited=$((waited + 1))
	done
	evidence "the unit was active for ${waited}s after the stand-in said it was ready"
	[ -n "$stopped" ] || note "the unit was still active ${boot_deadline}s after ready"

	log=$(journalctl "_SYSTEMD_INVOCATION_ID=$invocation" --no-pager --output cat 2>/dev/null || true)
	exits=$(printf '%s\n' "$log" | grep -c 'bootstrap exited, powering off' || true)
	errors=$(printf '%s\n' "$log" | grep -c '"level":"ERROR"' || true)
	evidence "jorb-init logged $exits exit events and $errors errors"
	[ "$exits" -eq 1 ] || note "expected one 'bootstrap exited' event, saw $exits"
	[ "$errors" -eq 0 ] || note "jorb-init logged an error on the way out"

	job delete "$orb" || note "the cleanup delete failed"
	wipe_everything
	expect_no_debris || note "something was left behind"
	finish_section 9 "an orb whose bootstrap exits powers off and its unit stops"
}

# ------------------------------------------ 10. no policy, no orb

section_10() {
	section 10 "without the network policy's table, spawn refuses and starts nothing"

	wipe_everything
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"

	local orb=m10 status=0

	# The table comes back from the module's ruleset at the end of the
	# section, whatever happened in it.
	nft delete table inet jorb || note "could not delete the table"
	job spawn "$orb" "$token" || status=$?
	systemctl restart nftables.service || note "could not restore the ruleset"
	module_installed || note "the table did not come back; the host is unconfined"

	evidence "spawn without the table exited $status"
	[ "$status" -ne 0 ] || note "spawn succeeded without the network policy"
	if ! tail -n 5 "$work/jorb.log" | grep -q 'inet jorb is not loaded'; then
		note "the failure did not name the table"
	fi
	if systemctl is-active --quiet "$(unit_of "$orb")"; then
		note "a unit was started anyway"
	fi

	job delete "$orb" || note "the cleanup delete failed"
	wipe_everything
	expect_no_debris || note "something was left behind"
	finish_section 10 "without the network policy's table, spawn refuses and starts nothing"
}

# ------------------------------------------------------------------ the whole

setup

requested=("$@")
if [ "${#requested[@]}" -eq 0 ]; then
	requested=(1 2 3 4 5 6 7 8 9 10)
fi

for number in "${requested[@]}"; do
	"section_$number"
done

printf '\n'
if [ "$failures" -ne 0 ]; then
	fail "$failures of $sections_run sections failed; jorb's log is at $work/jorb.log"
fi

pass "all $sections_run sections"
