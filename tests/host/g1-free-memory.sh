#!/usr/bin/env bash
# G1: how much memory is free with the observability stack idle, and therefore
# what services.jorb.memoryHigh should be.
#
# The answer is the slice's ceiling: free memory minus half a gigabyte of
# headroom for the host itself.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

headroom_kib=$((512 * 1024))

total_kib=$(awk '/^MemTotal:/ {print $2}' /proc/meminfo)
available_kib=$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)
recommended_kib=$((available_kib - headroom_kib))

evidence "MemTotal      $((total_kib / 1024)) MiB"
evidence "MemAvailable  $((available_kib / 1024)) MiB"
evidence "headroom      $((headroom_kib / 1024)) MiB"
evidence "running units in jorb.slice: $(systemctl list-units 'jorb-*' --no-legend --plain 2>/dev/null | wc -l)"
evidence "zram: $(test -e /sys/block/zram0/disksize && cat /sys/block/zram0/disksize || echo absent)"

if [ "$recommended_kib" -le 0 ]; then
	fail "only $((available_kib / 1024)) MiB available, which leaves nothing for a guest"
fi

evidence "services.jorb.memoryHigh = \"$((recommended_kib / 1024))M\""
pass "$((available_kib / 1024)) MiB available, so memoryHigh is $((recommended_kib / 1024))M"
