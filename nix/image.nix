# jorb-prepare-image: turns the pinned Amp orb image into the base image every
# orb's disk is cloned from. Root, because ownership and setuid bits have to
# survive into the ext4. Everything happens in a scratch directory and the
# result is renamed into place, so a kill leaves no base or a whole one.
#
# The `latest` tag moves often, so the digest is pinned; tests/host/amp-image.sh
# prints the current one.
{
  writeShellApplication,
  e2fsprogs,
  jq,
  skopeo,
  umoci,
  jorb-init,
}:

writeShellApplication {
  name = "jorb-prepare-image";
  runtimeInputs = [
    e2fsprogs
    jq
    skopeo
    umoci
  ];
  text = ''
    image=docker://us-central1-docker.pkg.dev/amp-prod-0/amp-orb/amp-orb-arm64@sha256:e25e30674cc051a6477fe9b148da1627f4186828dbd18e3151deeaa0d8ce1e68
    state=''${JORB_STATE_DIR:-/var/lib/jorb}
    work=$state/.image

    [ -e "$state/base.raw" ] && exit 0
    rm -rf "$work"
    mkdir -p "$work"

    # The digest is the integrity check, so no signature policy.
    skopeo --insecure-policy copy --quiet "$image" "oci:$work/oci:amp"
    umoci unpack --image "$work/oci:amp" "$work/bundle"
    rootfs=$work/bundle/rootfs

    install -m 0755 ${jorb-init}/bin/jorb-init "$rootfs/jorb-init"
    skopeo inspect --config "oci:$work/oci:amp" | jq -r '.config.Env // [] | .[]' >"$rootfs/etc/jorb-env"

    # Sparse: 64 GiB to the guest, what the image uses to the host.
    truncate -s 64G "$work/base.raw"
    mkfs.ext4 -q -F -L jorb -d "$rootfs" "$work/base.raw"
    sync "$work/base.raw"
    mv "$work/base.raw" "$state/base.raw"
    sync "$state"
  '';
}
