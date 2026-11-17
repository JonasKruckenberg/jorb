# shellcheck shell=bash
# The body of jorb-prepare-image. The wrapper around it sets
# image_repository, image_digest, jorb_init and base_size.
#
# Runs as root, once per host, before the provider starts. Everything happens
# under a scratch directory next to the base image and is renamed into place
# at the end, so a kill at any point leaves either the old base or no base,
# never half of one.

# The wrapper sets these four; naming them here is what makes the body a
# script you can read on its own.
image_repository=${image_repository:?set by the jorb-prepare-image wrapper}
image_digest=${image_digest:?set by the jorb-prepare-image wrapper}
jorb_init=${jorb_init:?set by the jorb-prepare-image wrapper}
base_size=${base_size:?set by the jorb-prepare-image wrapper}

state_directory=${JORB_STATE_DIR:-/var/lib/jorb}
base=$state_directory/base.raw
work=$state_directory/.image

if [ -e "$base" ]; then
	echo "base image already at $base"
	exit 0
fi

if [ "$(id -u)" -ne 0 ]; then
	echo "jorb-prepare-image needs root: the image's ownership and setuid bits have to survive into the ext4" >&2
	exit 1
fi

mkdir -p "$state_directory"
rm -rf "$work"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT

echo "pulling $image_repository@$image_digest"
# No signature policy: the digest is the integrity check.
skopeo --insecure-policy copy --quiet \
	"docker://$image_repository@$image_digest" \
	"oci:$work/oci:amp"

umoci unpack --image "$work/oci:amp" "$work/bundle"
rootfs=$work/bundle/rootfs

# The guest's PID 1, which the kernel is told to run as init=/jorb-init.
install -m 0755 "$jorb_init" "$rootfs/jorb-init"

# The image's own environment, for jorb-init to hand to bootstrap. Read from
# the local copy, so this needs the network only once.
mkdir -p "$rootfs/etc"
skopeo inspect --config "oci:$work/oci:amp" |
	jq -r '.config.Env // [] | .[]' >"$rootfs/etc/jorb-env"
echo "wrote $(grep -c '' "$rootfs/etc/jorb-env") variables to /etc/jorb-env"

# The guest's hostname is its orb id, set from the ip= cmdline, and it has to
# resolve: tools that look themselves up should not wait for DNS that cannot
# answer. 127.0.0.2 keeps it off the loopback name.
cat >"$rootfs/etc/hosts" <<'HOSTS'
127.0.0.1	localhost
::1		localhost ip6-localhost ip6-loopback
HOSTS

# A sparse file: the ext4 inside it is 64 GiB, the bytes on disk are what the
# image actually uses, and every orb's disk is a reflink clone of it.
truncate -s "$base_size" "$work/base.raw"
mkfs.ext4 -q -F -L jorb -d "$rootfs" "$work/base.raw"

# Rename, then fsync the directory, so the base either exists completely or
# not at all.
mv "$work/base.raw" "$base"
sync "$state_directory"

echo "base image at $base ($(du -h --apparent-size "$base" | cut -f1) apparent, $(du -h "$base" | cut -f1) used)"
