# The ext4 image G4 and G5 boot: busybox, a static init, nothing else.
#
# Its init reads `jorb.test=` from the kernel cmdline and prints one marker per
# thing a gate wants to observe, so the gates read the journal rather than
# poking at the guest.
{
  lib,
  stdenvNoCC,
  pkgsStatic,
  e2fsprogs,
}:

let
  busybox = pkgsStatic.busybox;
in

stdenvNoCC.mkDerivation {
  name = "jorb-test-image";

  nativeBuildInputs = [ e2fsprogs ];

  dontUnpack = true;

  buildPhase = ''
    runHook preBuild

    root=$PWD/root
    mkdir -p "$root"/{bin,dev,etc,proc,run,sys,tmp}
    cp ${busybox}/bin/busybox "$root/bin/busybox"
    ln -s busybox "$root/bin/sh"
    cp ${./test-init.sh} "$root/init"
    chmod +x "$root/init"

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p "$out"
    # Big enough for the guest to write, small enough to build quickly. The
    # real base image is 64 GiB and sparse.
    truncate -s 512M "$out/test.raw"
    mkfs.ext4 -q -F -L jorb-test -d "$PWD/root" "$out/test.raw"

    runHook postInstall
  '';

  meta = {
    description = "busybox ext4 image for jorb's host gates";
    platforms = lib.platforms.linux;
  };
}
