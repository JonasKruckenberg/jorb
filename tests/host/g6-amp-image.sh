#!/usr/bin/env bash
# G6: the Amp orb image is what jorb-init expects, and what `amp orb
# bootstrap` does when it starts.
#
# Pulls the arm64 image, prints the digest to pin in
# nix/image.nix, unpacks it rootless, checks A7 (glibc, setpriv, the user
# `user`, /usr/local/bin/amp) and then runs bootstrap with a dummy token to
# see whether it stays in the foreground, which is A8.

here=$(dirname "$0")
# shellcheck source-path=SCRIPTDIR
. "$here/lib.sh"

image=${JORB_AMP_IMAGE:-us-central1-docker.pkg.dev/amp-prod-0/amp-orb/amp-orb-arm64:latest}

work=${JORB_GATE_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/g6-XXXXXX")}
mkdir -p "$work"

# shellcheck disable=SC2329 # clean runs from the EXIT trap below
#
# A rootless unpack keeps the image's read-only directories read-only, and
# this image carries a nix store full of them. Set JORB_GATE_KEEP to look
# around the unpacked image afterwards; it is several gigabytes.
clean() {
	if [ -n "${JORB_GATE_KEEP:-}" ]; then
		printf '    kept     %s\n' "$work"
		return
	fi
	chmod -R u+w "$work" 2>/dev/null || true
	rm -rf "$work"
}
trap clean EXIT

# Follows a symlink chain the way the guest would: targets resolve inside the
# image, not against the host's root.
resolve_in_image() {
	local path=$1 hops=0 target
	while [ -L "$rootfs$path" ] && [ "$hops" -lt 10 ]; do
		target=$(readlink "$rootfs$path")
		case $target in
		/*) path=$target ;;
		*) path=$(dirname "$path")/$target ;;
		esac
		hops=$((hops + 1))
	done
	printf '%s' "$path"
}

evidence "image    $image"

digest=$(skopeo inspect --format '{{.Digest}}' "docker://$image")
evidence "digest   $digest"
evidence "pin this digest in nix/image.nix"

# No signature policy: the digest above is the integrity check, and it is
# what nix/image.nix pins.
skopeo --insecure-policy copy --quiet "docker://${image%%:*}@$digest" "oci:$work/oci:amp"
# umoci says a line per file about xattrs it cannot set as a user; the log is
# there if the unpack itself goes wrong.
umoci unpack --rootless --image "$work/oci:amp" "$work/bundle" >"$work/unpack.log" 2>&1
rootfs=$work/bundle/rootfs

evidence "unpacked $(du -sh "$rootfs" | cut -f1)"

missing=()

if [ ! -e "$rootfs/etc/passwd" ]; then
	missing+=(/etc/passwd)
else
	evidence "present  /etc/passwd"
fi

# /usr/local/bin/amp is what jorb-init execs. In this image it is a symlink
# into the default user's home, and Amp replaces the binary behind it on
# startup, so a dangling link is worth reporting rather than passing over.
amp_path=$(resolve_in_image /usr/local/bin/amp)
if [ -L "$rootfs/usr/local/bin/amp" ]; then
	evidence "amp      /usr/local/bin/amp -> $amp_path (inside the image)"
fi
if [ -f "$rootfs$amp_path" ]; then
	evidence "present  $amp_path"
else
	evidence "absent   $amp_path, so the image ships the link without the binary"
	missing+=("the binary behind /usr/local/bin/amp")
fi
evidence "home     $(find "$rootfs/home/user" -maxdepth 2 -name '.amp*' -o -maxdepth 2 -name 'bin' 2>/dev/null | sed "s|$rootfs||" | tr '\n' ' ')"

# setpriv is what jorb-init drops privileges with, wherever the image keeps
# it. This image is built with Nix, so /usr/bin is not the only candidate.
setpriv=$(find "$rootfs/usr/bin" "$rootfs/bin" "$rootfs/usr/local/bin" "$rootfs/sbin" \
	-maxdepth 1 -name setpriv 2>/dev/null | head -n 1)
if [ -n "$setpriv" ]; then
	evidence "present  ${setpriv#"$rootfs"} -> $(readlink -f "$setpriv" 2>/dev/null | sed "s|$rootfs||")"
else
	missing+=(setpriv)
fi

# glibc, not musl: jorb-init execs the image's own binaries, so the loader the
# amp binary asks for has to be inside the image. Ask the binary rather than
# guessing a path, because a Nix-built image keeps its loader in the store.
amp=$rootfs$amp_path
description=$(file -b "$amp" 2>/dev/null || echo unreadable)
evidence "amp      $description"
loader=$(printf '%s' "$description" | grep -o 'interpreter [^,]*' | cut -d' ' -f2- || true)
if [ -n "$loader" ] && [ -e "$rootfs$loader" ]; then
	evidence "loader   $loader, present in the image"
	loader=$rootfs$loader
elif printf '%s' "$description" | grep -q 'statically linked'; then
	evidence "amp is statically linked, so it needs no loader"
	loader=
else
	missing+=("the loader $loader the amp binary asks for")
	loader=
fi

if grep -q '^user:' "$rootfs/etc/passwd" 2>/dev/null; then
	evidence "user     $(grep '^user:' "$rootfs/etc/passwd")"
else
	missing+=("the user 'user' in /etc/passwd")
fi

if [ "${#missing[@]}" -ne 0 ]; then
	fail "the image is missing ${missing[*]}, so A7 does not hold"
fi

# A8: does bootstrap stay in the foreground? Run it through the image's own
# loader with a token that cannot work, and watch what the process does. The
# answer we need is about process shape, not about the token.
if [ -n "$loader" ]; then
	# The image's libraries, not the host's: the host is NixOS and has no
	# /usr/lib for this binary to find.
	libraries=$rootfs/lib:$rootfs/usr/lib
	for directory in "$rootfs"/lib/*-linux-gnu "$rootfs"/usr/lib/*-linux-gnu; do
		if [ -d "$directory" ]; then
			libraries=$libraries:$directory
		fi
	done

	set +e
	ORB_TOKEN=g6-dummy-token \
		timeout 30 "$loader" --library-path "$libraries" "$amp" orb bootstrap \
		>"$work/bootstrap.log" 2>&1 &
	bootstrap=$!
	sleep 10
	if kill -0 "$bootstrap" 2>/dev/null; then
		evidence "bootstrap still running after 10s, children: $(pgrep -P "$bootstrap" | tr '\n' ' ')"
		forked=no
	else
		wait "$bootstrap"
		status=$?
		evidence "bootstrap exited with $status in under 10s"
		forked=$status
	fi
	kill "$bootstrap" 2>/dev/null
	wait "$bootstrap" 2>/dev/null
	set -e

	evidence "bootstrap said:"
	head -n 5 "$work/bootstrap.log" | sed 's/^/      /'

	# Exiting non-zero with its own message is the foreground behaviour A8
	# needs: it did the work and reported, rather than detaching and leaving
	# PID 1 with nothing to wait for. A zero exit in under a second would be
	# the opposite.
	if [ "$forked" = 0 ]; then
		fail "bootstrap returned 0 immediately, which looks like it forked away, so A8 does not hold"
	fi
fi

pass "the image has glibc, setpriv, the user \`user\` and /usr/local/bin/amp; digest $digest"
