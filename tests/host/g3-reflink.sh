#!/usr/bin/env bash
# G3: /var/lib/jorb is XFS and a reflink copy works there.
#
# Every disk jorb hands to an orb is a reflink clone of the base image, so a
# filesystem without reflinks would turn every spawn into a 64 GiB copy.
# Creating the partition is the owner's job; this only reports what is there.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

state=${JORB_STATE_DIR:-/var/lib/jorb}

if [ ! -d "$state" ]; then
	fail "$state does not exist; the owner creates the XFS partition for it"
fi

filesystem=$(findmnt --noheadings --output FSTYPE --target "$state")
source=$(findmnt --noheadings --output SOURCE --target "$state")

evidence "$state is $filesystem on $source"
evidence "mount options: $(findmnt --noheadings --output OPTIONS --target "$state")"

if [ "$filesystem" != xfs ]; then
	fail "$state is $filesystem, not xfs, so reflinks are not available"
fi

work=$(mktemp -d "$state/.g3-XXXXXX")
trap 'rm -rf "$work"' EXIT

dd if=/dev/urandom of="$work/source" bs=1M count=8 status=none

if ! cp --reflink=always "$work/source" "$work/clone" 2>"$work/error"; then
	evidence "cp said: $(cat "$work/error")"
	fail "cp --reflink=always failed on $state"
fi

if ! cmp -s "$work/source" "$work/clone"; then
	fail "the reflink clone does not match its source"
fi

evidence "8 MiB reflink clone succeeded and matches"
evidence "blocks used by source: $(du -k "$work/source" | cut -f1) KiB, clone: $(du -k "$work/clone" | cut -f1) KiB"
pass "$state is xfs and cp --reflink=always works"
