#!/usr/bin/env bash
# What jorb does, checked against what AGENTS.md says it should, with the test
# image and a dummy token. Needs root, KVM and the module installed: jorb
# starts no orb without the network policy's table.
#
# Usage: sudo -E tests/host/matrix.sh [section...]   (all ten by default)

# shellcheck disable=SC2329 # sections are called by name
# shellcheck disable=SC2086 # orb names, the token and deadlines have no spaces
# shellcheck source-path=SCRIPTDIR
. "$(dirname "$0")/lib.sh"
need_root

# setup wipes this, so it must not be the production state, and it must be
# on XFS for reflinks.
work=${JORB_MATRIX_DIR:-/var/lib/jorb/matrix}
[ "$work" != /var/lib/jorb ] || fail "JORB_MATRIX_DIR would wipe the production state"
nft list table inet jorb >/dev/null 2>&1 || fail "the inet jorb table is not loaded; install the module"

# One slot, because section 7 is about contention for it.
export JORB_SLOTS=1 JORB_VCPUS=2 JORB_MEMORY_MIB=2048
export JORB_STATE_DIR=$work/state JORB_RUN_DIR=/run/jorb-matrix
JORB_CH_BIN=$(command -v cloud-hypervisor)
JORB_KERNEL=$(built .#guest-kernel)/kernel
export JORB_CH_BIN JORB_KERNEL
jorb=${JORB_BIN:-$(built .#jorb)/bin/jorb}
token=matrix-dummy-token
probe_tcp_port=47123
# LOCK_WAIT plus JOB_DEADLINE in jorb/src/limits.rs; change it with them.
job_deadline=280
boot_deadline=90
failures=0

# ------------------------------------------------------------------ harness

job() {
	local operation=$1 orb=$2 job_token=${3:-}
	env ORB_OPERATION="$operation" ORB_ID="$orb" ${job_token:+ORB_TOKEN="$job_token"} \
		timeout 300 "$jorb" 2>>"$work/jorb.log"
}

unit_of() { printf 'jorb-orb-%s.service' "$1"; }
orb_units() { systemctl list-units --all --plain --no-legend 'jorb-orb-*.service' | awk '{print $1}'; }
invocation_of() { systemctl show -p InvocationID --value "$(unit_of "$1")"; }
# One unit invocation's journal, which outlives the unit.
journal_of() {
	[ -n "$1" ] || return 0
	journalctl "_SYSTEMD_INVOCATION_ID=$1" --no-pager --output cat 2>/dev/null || true
}
logged() { journal_of "$1" | grep -q "JORB-GATE $2"; }

# Unit, VM and disk as three words, observed without jorb.
observed() {
	local orb=$1 unit=inactive vm=no-socket disk=absent
	systemctl is-active --quiet "$(unit_of "$orb")" && unit=active
	if [ -S "$JORB_RUN_DIR/$orb.sock" ]; then
		vm=$(ch-remote --api-socket="$JORB_RUN_DIR/$orb.sock" info 2>/dev/null | jq -r .state 2>/dev/null)
		vm=${vm:-unreachable}
	fi
	[ -f "$JORB_STATE_DIR/disks/$orb.raw" ] && disk=present
	printf '%s %s %s' "$unit" "$vm" "$disk"
}

# The state an operation converges on. Pause keeps whatever disk there was.
wanted_for() {
	case $1 in
	spawn) printf 'active Running present' ;;
	pause) printf 'inactive no-socket %s' "${2:-present}" ;;
	delete) printf 'inactive no-socket absent' ;;
	esac
}

expect() {
	local actual
	actual=$(observed "$1")
	[ "$actual" = "$2" ] || note "$1 is '$actual', expected '$2'"
}

# Nothing a later job would trip over, except what a running orb may hold.
expect_no_debris() {
	local allowed=${1:-} unit path
	for unit in $(orb_units); do
		[ "$unit" = "$(unit_of "$allowed")" ] || note "unit left behind: $unit"
	done
	for path in /sys/class/net/jorb[0-9]* "$JORB_RUN_DIR"/*.sock "$JORB_RUN_DIR"/*.cfg "$JORB_STATE_DIR/disks/.tmp"; do
		[ -e "$path" ] || continue
		case $path in
		/sys/*) [ -n "$allowed" ] && continue ;;
		"$JORB_RUN_DIR/$allowed".*) continue ;;
		esac
		note "left behind: $path"
	done
}

# Polls a command once a second until it succeeds or the deadline passes.
within() {
	local deadline=$1 waited=0
	shift
	until "$@"; do
		[ "$waited" -lt "$deadline" ] || return 1
		sleep 1
		waited=$((waited + 1))
	done
}
running() { [ "$(observed "$1")" = "active Running present" ]; }
stopped() { ! systemctl is-active --quiet "$(unit_of "$1")"; }

wipe() {
	local unit tap
	for unit in $(orb_units); do systemctl stop "$unit" >/dev/null 2>&1 || true; done
	for tap in /sys/class/net/jorb[0-9]*; do
		ip link del "$(basename "$tap")" 2>/dev/null || true
	done
	rm -rf "$JORB_RUN_DIR" "$JORB_STATE_DIR"/disks/*.raw "$JORB_STATE_DIR/disks/.tmp"
	mkdir -p "$JORB_RUN_DIR" "$JORB_STATE_DIR/disks"
}

section_failed=
note() {
	printf '      %s\n' "$*"
	section_failed=yes
}

setup() {
	rm -rf "$work"
	mkdir -p "$JORB_STATE_DIR/disks" "$JORB_RUN_DIR"
	install -m 0600 "$(built .#test-image)/test.raw" "$JORB_STATE_DIR/base.raw"
	cp --reflink=always "$JORB_STATE_DIR/base.raw" "$work/probe" ||
		fail "$work cannot reflink; point JORB_MATRIX_DIR at the XFS mount"
	rm -f "$work/probe"

	# Addresses only the host knows, for the stand-in to probe.
	local route
	route=$(ip -o -4 route show default | head -n 1)
	printf 'JORB_PROBE_HOST_LAN=%s\nJORB_PROBE_LAN_NEIGHBOUR=%s\nJORB_PROBE_HOST_TCP_PORT=%s\n' \
		"$(grep -o 'src [0-9.]*' <<<"$route" | cut -d' ' -f2)" \
		"$(grep -o 'via [0-9.]*' <<<"$route" | cut -d' ' -f2)" \
		"$probe_tcp_port" >"$work/probe-targets"
	debugfs -w -R "write $work/probe-targets /etc/jorb-probe-targets" "$JORB_STATE_DIR/base.raw" >/dev/null
}

# ----------------------------------------------------------------- sections

# 1. Every operation from every start state, twice.
section_1() {
	local orb=m1 start operation attempt disk
	for start in absent stopped running; do
		for operation in spawn pause delete; do
			wipe
			disk=present
			if [ "$start" = absent ]; then
				disk=absent
			else
				job spawn $orb $token || note "$start: setup spawn failed"
			fi
			if [ "$start" = stopped ]; then job pause $orb || note "$start: setup pause failed"; fi

			for attempt in first second; do
				job "$operation" $orb $token || note "$start/$operation ($attempt) failed"
				expect $orb "$(wanted_for "$operation" "$disk")"
			done
			if [ "$operation" = spawn ]; then expect_no_debris $orb; else expect_no_debris; fi
			evidence "$start/$operation -> $(observed $orb)"
		done
	done
}

# 2. Fifty jobs killed at random points, then convergence.
section_2() {
	local orb=m2 round delay operation
	for round in $(seq 1 50); do
		# Up to 1.5 s, so a kill lands anywhere from the lock to mid-boot. Never
		# 0, which timeout reads as no timeout at all.
		delay=$(awk -v seed="$round" 'BEGIN { srand(seed); printf "%.2f", 0.01 + rand() * 1.5 }')
		ORB_OPERATION=$(shuf -e spawn pause delete -n 1) ORB_ID=$orb ORB_TOKEN=$token \
			timeout -s KILL "$delay" "$jorb" 2>/dev/null || true
	done
	for operation in spawn pause delete; do
		job $operation $orb $token || note "$operation after the kills failed"
		expect $orb "$(wanted_for $operation)"
	done
	expect_no_debris
}

# 3. An identical spawn leaves a healthy orb alone; a new token restarts it.
section_3() {
	local orb=m3 first identical changed
	job spawn $orb $token || note "spawn failed"
	first=$(invocation_of $orb)
	job spawn $orb $token || note "identical spawn failed"
	identical=$(invocation_of $orb)
	job spawn $orb "$token-changed" || note "changed spawn failed"
	changed=$(invocation_of $orb)
	[ "$first" = "$identical" ] || note "an identical spawn restarted the VM"
	[ "$first" != "$changed" ] || note "a changed token did not restart the VM"
}

# 4. The guest is set up as AGENTS.md says, and reaches the internet and
# nothing else. Also records how long a boot takes, and where the kernel
# spent it.
section_4() {
	local orb=m4 listener started invocation checks
	systemd-socket-activate --listen="0.0.0.0:$probe_tcp_port" --accept cat >/dev/null 2>&1 &
	listener=$!
	started=$(date +%s)
	job spawn $orb $token || note "spawn failed"
	invocation=$(invocation_of $orb)
	within $boot_deadline logged "$invocation" ready || note "the guest never got to ready"
	kill "$listener" 2>/dev/null || true

	evidence "spawn to ready: $(($(date +%s) - started))s"
	evidence "kernel reached init: $(journal_of "$invocation" | grep -m1 'Run /jorb-init' || echo never)"
	evidence "the line before it: $(journal_of "$invocation" | grep -B1 -m1 'Run /jorb-init' | head -n 1)"
	checks=$(journal_of "$invocation" | grep -o 'JORB-GATE check .*' || true)
	evidence "$(grep -c ' ok$' <<<"$checks") checks passed"
	while read -r failed; do [ -z "$failed" ] || note "$failed"; done <<<"$(grep ' FAILED' <<<"$checks")"
	grep -q 'check internet ok' <<<"$checks" || note "the guest did not report reaching ampcode.com"
}

# 5. Memory the orb frees comes back to the host.
section_5() {
	local orb=m5 invocation cgroup peak=0 current settled
	job spawn $orb "workload-memory-$token" || note "spawn failed"
	invocation=$(invocation_of $orb)
	cgroup=/sys/fs/cgroup$(systemctl show -p ControlGroup --value "$(unit_of $orb)")
	within $boot_deadline logged "$invocation" 'memory allocating' || note "the workload never started"
	for _ in $(seq 1 30); do
		current=$(cat "$cgroup/memory.current")
		[ "$current" -gt "$peak" ] && peak=$current
		logged "$invocation" 'memory freed' && break
		sleep 2
	done
	sleep 30 # free page reporting works in batches
	settled=$(cat "$cgroup/memory.current")
	evidence "peak $((peak >> 20)) MiB, settled $((settled >> 20)) MiB"
	evidence "zram original and compressed bytes: $(cut -d' ' -f1,2 /sys/block/zram0/mm_stat 2>/dev/null || echo 'no zram')"
	[ "$settled" -lt "$peak" ] || note "the cgroup did not fall after the guest freed 900 MiB"
}

# 6. A VMM that never opens its socket fails the spawn within API_SOCKET_WAIT,
# and the next job of any orb removes it. (Section 8 covers one that stops
# answering.)
section_6() {
	local orb=m6 fake=$work/fake started status=0
	mkdir -p "$fake"
	printf '#!/bin/sh\nexec sleep 600\n' >"$fake/cloud-hypervisor"
	chmod +x "$fake/cloud-hypervisor"

	started=$(date +%s)
	JORB_CH_BIN=$fake/cloud-hypervisor job spawn $orb $token || status=$?
	evidence "spawn exited $status after $(($(date +%s) - started))s"
	[ "$status" -ne 0 ] || note "a spawn whose VMM never answered succeeded"
	[ $(($(date +%s) - started)) -le 40 ] || note "it took too long to give up"

	job pause m6b || note "the next job failed"
	stopped $orb || note "the next job left the unit"
	expect_no_debris
}

# 7. With one slot: a killed VMM comes back on the same disk, and a second orb
# gets the slot only once the first is gone.
section_7() {
	local first=m7a second=m7b inode
	job spawn $first $token || note "spawn of $first failed"
	inode=$(stat -c %i "$JORB_STATE_DIR/disks/$first.raw")
	pkill -9 -f "$JORB_RUN_DIR/$first.sock" || true
	sleep 2
	job spawn $first $token || note "spawn after the kill failed"
	expect $first "$(wanted_for spawn)"
	[ "$(stat -c %i "$JORB_STATE_DIR/disks/$first.raw")" = "$inode" ] || note "it came back on a new disk"

	if job spawn $second $token; then note "$second started while $first held the only slot"; fi
	job delete $first || note "delete of $first failed"
	job spawn $second $token || note "$second did not take the freed slot"
	expect $second "$(wanted_for spawn)"
}

# 8. A stopped VMM does not block the delete that clears it.
section_8() {
	local started status=0
	job spawn m8a $token || note "spawn failed"
	pkill -STOP -f "$JORB_RUN_DIR/m8a.sock" || note "no VMM to stop"
	started=$(date +%s)
	job delete m8a || status=$?
	evidence "delete exited $status after $(($(date +%s) - started))s"
	[ "$status" -eq 0 ] || note "the delete failed"
	[ $(($(date +%s) - started)) -le $job_deadline ] || note "the delete overran the job deadline"
	job spawn m8b $token || note "the next spawn failed"
	expect m8b "$(wanted_for spawn)"
}

# 9. When bootstrap exits the guest powers off, saying so once, without error.
section_9() {
	local orb=m9 invocation log
	job spawn $orb "$token-exit-bootstrap" || note "spawn failed"
	# Read now: the unit is collected seconds after the guest is ready.
	invocation=$(invocation_of $orb)
	within $boot_deadline stopped $orb || note "the unit did not stop"
	log=$(journal_of "$invocation")
	[ "$(grep -c 'bootstrap exited, powering off' <<<"$log")" -eq 1 ] || note "expected one exit event"
	if grep -q '"level":"ERROR"' <<<"$log"; then note "jorb-init logged an error"; fi
}

# 10. Without the policy table, spawn refuses and starts nothing. Only jorb's
# table is removed and restored.
section_10() {
	local saved=$work/jorb-table.nft
	nft list table inet jorb >"$saved"
	nft delete table inet jorb
	if job spawn m10 $token; then note "spawn succeeded without the policy"; fi
	nft -f "$saved" || note "COULD NOT RESTORE inet jorb: orbs on this host are unconfined"
	tail -n 3 "$work/jorb.log" | grep -q 'inet jorb is not loaded' || note "the failure did not name the table"
	stopped m10 || note "a unit was started anyway"
}

# --------------------------------------------------------------------- main

setup
sections=("$@")
[ "${#sections[@]}" -gt 0 ] || sections=(1 2 3 4 5 6 7 8 9 10)
for number in "${sections[@]}"; do
	printf '\n--- %s\n' "$number"
	section_failed=
	# A command that fails inside a section is a finding, not a reason to stop
	# the run: the section's checks report it.
	set +e
	wipe
	"section_$number"
	wipe
	set -e
	if [ -n "$section_failed" ]; then
		printf 'FAIL matrix %s\n' "$number"
		failures=$((failures + 1))
	else
		printf 'PASS matrix %s\n' "$number"
	fi
done
[ "$failures" -eq 0 ] || fail "$failures of ${#sections[@]} sections failed; jorb's log is $work/jorb.log"
pass "all ${#sections[@]} sections"
