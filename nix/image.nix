# jorb-prepare-image: turns the pinned Amp orb image into /var/lib/jorb/base.raw.
#
# Every orb's disk is a reflink clone of that file, so this runs once per host
# (as the provider service's ExecStartPre) and exits immediately when the base
# is already there. It needs root: the image's ownership and setuid bits have
# to survive into the ext4, and mkfs.ext4 -d keeps whatever the unpack left.
#
# The digests below are what the gate records. Refresh them by running
# tests/host/g6-amp-image.sh, which prints the current one; the `latest` tags
# move often, which is the reason jorb pins a digest rather than a tag.
{
  lib,
  stdenv,
  writeShellApplication,
  coreutils,
  e2fsprogs,
  jq,
  skopeo,
  umoci,
  util-linux,
  jorb-init,
}:

let
  registry = "us-central1-docker.pkg.dev/amp-prod-0/amp-orb";

  images = {
    aarch64 = {
      repository = "${registry}/amp-orb-arm64";
      # Recorded by G6 on 2026-11-18.
      digest = "sha256:e25e30674cc051a6477fe9b148da1627f4186828dbd18e3151deeaa0d8ce1e68";
    };
    x86_64 = {
      repository = "${registry}/amp-orb";
      # Recorded with skopeo inspect --override-arch amd64 on 2026-11-18;
      # awaiting a G6 run on an x86_64 host.
      digest = "sha256:a431746a91926c0686942b73ab0ccfa03e23eb96f5a647612e5948ec4e68729b";
    };
  };

  image = images.${stdenv.hostPlatform.parsed.cpu.name};
in

writeShellApplication {
  name = "jorb-prepare-image";

  runtimeInputs = [
    coreutils
    e2fsprogs
    jq
    skopeo
    umoci
    util-linux
  ];

  # Keep the script itself free of Nix interpolation beyond these four, so
  # shellcheck sees the same text that runs.
  text = ''
    image_repository=${lib.escapeShellArg image.repository}
    image_digest=${lib.escapeShellArg image.digest}
    jorb_init=${lib.escapeShellArg "${jorb-init}/bin/jorb-init"}
    base_size=${lib.escapeShellArg "64G"}

    ${builtins.readFile ./prepare-image.sh}
  '';

  meta = {
    description = "Builds jorb's base disk image from the pinned Amp orb image";
    platforms = lib.platforms.linux;
  };
}
