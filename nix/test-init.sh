#!/bin/sh
# PID 1 in the gates' busybox guest. Prints one marker per thing a gate
# observes, then does what jorb.test= asks for.
#
# Markers are prefixed JORB-GATE so a gate can grep the unit's journal for
# them. This is test scaffolding: the real guest init is jorb-init.

set -u

PATH=/bin
export PATH

mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs -o size=1G tmpfs /run

echo "JORB-GATE booted $(uname -r) $(uname -m)"
echo "JORB-GATE date $(date -u +%s)"

# A2: the root disk is whichever one the kernel mounted, so the question is
# whether the config disk is the second one.
for disk in /dev/vda /dev/vdb /dev/vdc; do
	if [ -b "$disk" ]; then
		echo "JORB-GATE disk $disk present"
	else
		echo "JORB-GATE disk $disk absent"
	fi
done
echo "JORB-GATE vdb-first-line $(head -c 64 /dev/vdb 2>/dev/null | tr -d '\000' | head -n 1)"

echo "JORB-GATE addr $(ip -o addr show eth0 2>/dev/null | tr -s ' ')"
echo "JORB-GATE resolv $(tr -d '\n' </proc/net/pnp 2>/dev/null)"

mode=boot
# Word splitting is the point here: the cmdline is a list of words.
# shellcheck disable=SC2013
for word in $(cat /proc/cmdline); do
	case $word in
	jorb.test=*) mode=${word#jorb.test=} ;;
	esac
done
echo "JORB-GATE mode $mode"

case $mode in
memory)
	# G5: take a gigabyte, hand it back, then stay up so the host can watch
	# the cgroup. tmpfs pages are guest-free once the file is gone, which is
	# what free page reporting is meant to return to the host.
	mount -t tmpfs -o size=2G tmpfs /tmp
	echo "JORB-GATE memory allocating"
	dd if=/dev/zero of=/tmp/blob bs=1M count=1024 2>/dev/null
	sync
	echo "JORB-GATE memory allocated $(free -m | tr -s ' ' | tr '\n' ';')"
	sleep 10
	rm -f /tmp/blob
	sync
	echo "JORB-GATE memory freed $(free -m | tr -s ' ' | tr '\n' ';')"
	# Idle long enough for the host to sample memory.current, then stop.
	sleep 180
	echo "JORB-GATE memory done"
	;;
*)
	echo "JORB-GATE boot done"
	sleep 5
	;;
esac

poweroff -f
