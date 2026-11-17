#!/bin/sh
# Stands in for `amp orb bootstrap` in the gates' guest. It reports what
# jorb-init did on its way here, then stays in the foreground, which is what
# G6 observed the real one does.

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

sleep 120
