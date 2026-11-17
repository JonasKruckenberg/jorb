#!/usr/bin/env bash
# G2: the host's PMD huge page size, which is the reason the guests run with
# transparent huge pages off (A6).
#
# A 16K-page host has a 32 MiB PMD, so a transparent huge page would be 32 MiB
# of memory the guest cannot hand back a page at a time.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

expected=33554432
path=/sys/kernel/mm/transparent_hugepage/hpage_pmd_size

if [ ! -r "$path" ]; then
	fail "$path is not readable, so A6 cannot be checked"
fi

size=$(cat "$path")

evidence "host page size   $(getconf PAGESIZE) bytes"
evidence "hpage_pmd_size   $size bytes ($(mib "$size"))"
evidence "THP enabled      $(cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || echo unknown)"

if [ "$size" -ne "$expected" ]; then
	fail "hpage_pmd_size is $size, not the $expected A6 assumes; ask before changing the memory shape"
fi

pass "hpage_pmd_size is $(mib "$size"), which is what A6 assumes"
