#!/usr/bin/env bash
# G4: the final VM shape boots through the API, and everything jorb relies on
# is true at once: the guest comes up, its tap is up, the disks arrive in
# attach order, its clock is right, and its console reaches the journal.
#
# Resolves A1, A2, A3 and A4. Boots the busybox test image, not an orb.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

need_root
need_kvm

slot=${JORB_SLOT:-0}
id=${JORB_GATE_ID:-g4}
init=${JORB_GATE_INIT:-/init}

trap stop_test_vm EXIT
start_test_vm "$id" "$slot" boot "$init"

if ! wait_for_marker 'mode' "${JORB_GATE_DEADLINE:-90}"; then
	evidence "last journal lines:"
	tail -n 20 "$gate_journal" | sed 's/^/      /'
	fail "the guest never reached its init, so A1 does not hold as the VM is shaped"
fi

evidence "guest booted: $(marker booted)"
evidence "console reached the journal after ${gate_marker_wait}s, so A3 holds"

# A2: attach order. The guest booted with root=/dev/vda, so vda is the root
# disk; the config disk has to be the one behind it.
vdb_line=$(marker vdb-first-line)
evidence "disks: $(grep -o '/dev/vd[abc] [a-z]*' "$gate_journal" | tr '\n' ' ')"
evidence "first line of /dev/vdb: $vdb_line"
if [ "$vdb_line" != "JORB_GATE=$id" ]; then
	fail "/dev/vdb is not the config disk, so A2 does not hold"
fi

# A4: the RTC should put the guest clock within seconds of the host's.
guest_epoch=$(marker date)
skew=$((guest_epoch - gate_boot_epoch))
evidence "guest clock $guest_epoch against host $gate_boot_epoch, skew ${skew}s"
if [ "${skew#-}" -gt 60 ]; then
	fail "the guest clock is ${skew}s off the host's, so A4 does not hold"
fi

if [ ! -d "/sys/class/net/$gate_tap" ]; then
	fail "$gate_tap does not exist, so the slot's tap was never created"
fi
evidence "tap      $gate_tap state $(cat "/sys/class/net/$gate_tap/operstate")"
evidence "guest addr line: $(marker addr)"
evidence "guest resolv: $(marker resolv)"
if ! marker addr | grep -q "$gate_guest_address"; then
	fail "the guest did not take $gate_guest_address from the ip= cmdline"
fi

evidence "unit cgroup: $(systemctl show -p ControlGroup --value "$gate_unit.service")"
pass "the VM shape boots: guest up, $gate_tap up, vdb is the config disk, clock within ${skew}s, console in the journal"
