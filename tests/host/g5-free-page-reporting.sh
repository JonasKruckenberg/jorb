#!/usr/bin/env bash
# G5: memory the guest frees comes back to the host.
#
# The guest takes a gigabyte of tmpfs, hands it back, and the unit's cgroup
# should shed most of it within a minute. Resolves A5, which is what keeps one
# slot's idle orb from holding the host's memory hostage.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

need_root
need_kvm

slot=${JORB_SLOT:-0}
id=${JORB_GATE_ID:-g5}
# How much of the gigabyte has to come back, and how long it may take.
required_return=${JORB_GATE_RETURN_MIB:-700}
window=${JORB_GATE_WINDOW:-60}

trap stop_test_vm EXIT
start_test_vm "$id" "$slot" memory /init

if ! wait_for_marker 'memory allocated' "${JORB_GATE_DEADLINE:-120}"; then
	evidence "last journal lines:"
	tail -n 20 "$gate_journal" | sed 's/^/      /'
	fail "the guest never allocated its gigabyte"
fi

allocated=$(unit_memory_current)
evidence "guest reported: $(marker 'memory allocated')"
evidence "cgroup after allocating: $(mib "$allocated")"

if ! wait_for_marker 'memory freed' "${JORB_GATE_DEADLINE:-120}"; then
	fail "the guest never freed its gigabyte"
fi

evidence "guest reported: $(marker 'memory freed')"

# Free page reporting is not instant: the balloon reports in batches.
waited=0
freed=$allocated
while [ "$waited" -lt "$window" ]; do
	freed=$(unit_memory_current)
	returned_mib=$(((allocated - freed) / 1024 / 1024))
	if [ "$returned_mib" -ge "$required_return" ]; then
		break
	fi
	sleep 5
	waited=$((waited + 5))
done

returned_mib=$(((allocated - freed) / 1024 / 1024))
evidence "cgroup after freeing:    $(mib "$freed")"
evidence "returned to the host:    ${returned_mib} MiB after ${waited}s"

if [ "$returned_mib" -lt "$required_return" ]; then
	fail "only ${returned_mib} MiB of the gigabyte came back within ${window}s, so A5 does not hold"
fi

pass "${returned_mib} MiB came back within ${waited}s, so free page reporting works"
