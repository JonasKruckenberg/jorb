#!/bin/sh
# Stands in for `amp orb bootstrap` in the test image. It checks from inside
# the guest what jorb-init and the network policy promise, one line each, then
# does what its token asks and stays in the foreground as the real one does.
# jorb itself has no hooks for any of this.

# shellcheck disable=SC2016 # each check is a string for eval to expand

check() {
	if eval "$2" >/dev/null 2>&1; then
		echo "JORB-GATE check $1 ok"
	else
		echo "JORB-GATE check $1 FAILED"
	fi
}

check user '[ "$(id -u):$(id -g)" = 1000:1000 ]'
check home '[ "$HOME" = /home/user ] && [ "$PWD" = /home/user ]'
check token '[ -n "$ORB_TOKEN" ]'
check resolv-conf 'grep -q nameserver /etc/resolv.conf'
check writeback '[ "$(cat /proc/sys/vm/dirty_expire_centisecs) $(cat /proc/sys/vm/dirty_writeback_centisecs)" = "100 100" ]'
for mountpoint in /proc /sys /dev/pts /dev/shm /run; do
	check "mount-$mountpoint" "grep -q ' $mountpoint ' /proc/mounts"
done
check dev-stderr 'echo probe >/dev/stderr'
check dev-fd '[ -e /dev/fd/0 ]'
# script opens the pty slave by its /dev/pts path.
if command -v script >/dev/null; then
	check pty 'script -q -c true /dev/null'
fi
check hostname 'ping -c 1 -W 2 "$(hostname)"'

# The policy: the host (the other end of the slot's /30), the LAN, metadata
# and private ranges are unreachable; Amp is reachable, by name.
# shellcheck disable=SC1091 # written into the matrix's copy of the image
[ -r /etc/jorb-probe-targets ] && . /etc/jorb-probe-targets
host=$(ip -o -4 addr show eth0 | awk '{split($4, a, "[./]"); print a[1]"."a[2]"."a[3]"."(a[4] - 1)}')
for target in "$host" ${JORB_PROBE_HOST_LAN:-} ${JORB_PROBE_LAN_NEIGHBOUR:-} \
	169.254.169.254 10.0.0.1 172.16.0.1 192.168.0.1; do
	check "unreachable-$target" "! ping -c 1 -W 2 $target"
done
if [ -n "${JORB_PROBE_HOST_TCP_PORT:-}" ]; then
	check unreachable-tcp "! nc -w 2 $host $JORB_PROBE_HOST_TCP_PORT </dev/null"
fi
check internet 'wget -q -T 20 -O /dev/null https://ampcode.com/'

case "$ORB_TOKEN" in
*workload-memory*)
	mount -t tmpfs -o size=2G tmpfs /tmp
	echo "JORB-GATE memory allocating"
	dd if=/dev/zero of=/tmp/blob bs=1M count=1024 2>/dev/null
	sleep 10
	rm -f /tmp/blob
	echo "JORB-GATE memory freed"
	;;
esac

echo "JORB-GATE ready"

case "$ORB_TOKEN" in
*exit-bootstrap*)
	sleep 2
	exit 3
	;;
esac

while true; do
	sleep 3600
done
