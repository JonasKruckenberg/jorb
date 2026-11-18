#!/bin/sh
# Stands in for `amp orb bootstrap` in the gates' guest.
#
# It reports what jorb-init did on its way here, probes the network policy
# from the inside, and then stays in the foreground, which is what G6
# observed the real bootstrap does. This is test scaffolding inside the test
# image: jorb itself has no hooks for any of it.
#
# The matrix picks behaviour with the token it spawns an orb with, because the
# token is the one value it can choose per job.

echo "JORB-GATE amp argv $*"
echo "JORB-GATE amp user $(id -u):$(id -g)"
echo "JORB-GATE amp home $HOME"
echo "JORB-GATE amp cwd $(pwd)"

if [ -n "${ORB_TOKEN:-}" ]; then
	echo "JORB-GATE amp token present"
else
	echo "JORB-GATE amp token missing"
fi

# Step 3 of jorb-init: the kernel's ip= answer copied into place.
echo "JORB-GATE amp resolv $(tr '\n' ' ' </etc/resolv.conf 2>/dev/null)"

# Step 2: the writeback window, which pause depends on being short.
echo "JORB-GATE amp dirty $(cat /proc/sys/vm/dirty_expire_centisecs 2>/dev/null)/$(cat /proc/sys/vm/dirty_writeback_centisecs 2>/dev/null)"

# Step 1: the mounts, as a list of what is mounted where.
echo "JORB-GATE amp mounts $(awk '{print $2}' /proc/mounts | tr '\n' ' ')"

# The guest's own address tells it the host's: a slot is a /30, and the host
# is the other usable address in it.
guest_address=$(ip -o -4 addr show eth0 2>/dev/null | awk '{print $4}' | cut -d/ -f1)
host_address=$(echo "$guest_address" | awk -F. '{print $1"."$2"."$3"."($4 - 1)}')
echo "JORB-GATE amp address $guest_address via $host_address"

# Anything else the matrix wants probed, written into its copy of the base
# image before the run. Optional: without it the generic ranges below still
# show whether the policy holds.
if [ -r /etc/jorb-probe-targets ]; then
	# shellcheck disable=SC1091 # written by tests/host/matrix.sh
	. /etc/jorb-probe-targets
fi

unreachable() {
	name=$1
	target=$2
	[ -n "$target" ] || return 0

	if ping -c 1 -W 2 "$target" >/dev/null 2>&1; then
		echo "JORB-GATE amp reach $name $target REACHED"
	else
		echo "JORB-GATE amp reach $name $target unreachable"
	fi
}

unreachable tap "$host_address"
unreachable host-lan "${JORB_PROBE_HOST_LAN:-}"
unreachable lan-neighbour "${JORB_PROBE_LAN_NEIGHBOUR:-}"
unreachable metadata 169.254.169.254
unreachable private-10 10.0.0.1
unreachable private-172 172.16.0.1
unreachable private-192 192.168.0.1

# The one thing an orb does need: Amp itself, over the internet, by name, so
# this covers DNS as well as routing.
if wget -q -T 20 -O /dev/null https://ampcode.com/ 2>/dev/null; then
	echo "JORB-GATE amp reach ampcode.com reachable"
else
	echo "JORB-GATE amp reach ampcode.com UNREACHABLE"
fi

# A build-like workload, for the section that watches the cgroup fall again.
case "${ORB_TOKEN:-}" in
*workload-memory*)
	mount -t tmpfs -o size=2G tmpfs /tmp 2>/dev/null
	echo "JORB-GATE amp memory allocating"
	dd if=/dev/zero of=/tmp/blob bs=1M count=1024 2>/dev/null
	sync
	echo "JORB-GATE amp memory allocated"
	sleep 10
	rm -f /tmp/blob
	sync
	echo "JORB-GATE amp memory freed"
	;;
esac

echo "JORB-GATE amp ready"

# Stay in the foreground for the life of the orb, as bootstrap does.
while true; do
	sleep 3600
done
