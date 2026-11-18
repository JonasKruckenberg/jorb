# The ext4 image the gates boot: busybox, a static init, and just enough of an
# orb image for jorb-init to run through all seven of its steps.
#
# Its init reads `jorb.test=` from the kernel cmdline and prints one marker per
# thing a gate wants to observe, so the gates read the journal rather than
# poking at the guest. Booting with `init=/jorb-init` instead runs the real
# guest init against a stand-in for `amp orb bootstrap`.
{
  lib,
  stdenvNoCC,
  pkgsStatic,
  e2fsprogs,
  jorb-init,
}:

let
  busybox = pkgsStatic.busybox;
  # busybox's setpriv cannot change uid; jorb-init calls util-linux's, which
  # the Amp orb image has at this same path (G6).
  utilLinux = pkgsStatic.util-linux;
in

stdenvNoCC.mkDerivation {
  name = "jorb-test-image";

  nativeBuildInputs = [ e2fsprogs ];

  dontUnpack = true;

  buildPhase = ''
    runHook preBuild

    root=$PWD/root
    mkdir -p "$root"/{bin,dev,etc,home/user,proc,run,sys,tmp,usr/bin,usr/local/bin}
    cp ${busybox}/bin/busybox "$root/bin/busybox"
    # Every applet, as a symlink, at build time, so a guest booted with
    # init=/jorb-init has the same tools as one booted with the test init.
    for applet in $(${busybox}/bin/busybox --list); do
      ln -sf busybox "$root/bin/$applet"
    done
    cp ${utilLinux}/bin/setpriv "$root/usr/bin/setpriv"
    cp ${./init.sh} "$root/init"
    cp ${jorb-init}/bin/jorb-init "$root/jorb-init"
    chmod +x "$root/init" "$root/jorb-init"

    # Who bootstrap runs as, named as the Amp orb image names them.
    printf '%s\n' \
      'root:x:0:0:root:/root:/bin/sh' \
      'user:x:1000:1000:user:/home/user:/bin/sh' >"$root/etc/passwd"
    printf '%s\n' 'root:x:0:' 'user:x:1000:' >"$root/etc/group"

    # What jorb-prepare-image writes from the image config's Env.
    echo 'PATH=/usr/local/bin:/usr/bin:/bin' >"$root/etc/jorb-env"

    cp ${./amp.sh} "$root/usr/local/bin/amp"
    chmod +x "$root/usr/local/bin/amp"

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
