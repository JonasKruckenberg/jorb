# The matrix's base image: busybox, jorb-init, and a stand-in for `amp orb
# bootstrap`, laid out as the Amp image lays them out.
{
  stdenvNoCC,
  pkgsStatic,
  e2fsprogs,
  jorb-init,
}:

stdenvNoCC.mkDerivation {
  name = "jorb-test-image";
  nativeBuildInputs = [ e2fsprogs ];
  dontUnpack = true;

  installPhase = ''
    root=$PWD/root
    mkdir -p "$root"/{bin,dev,etc,home/user,proc,run,sys,tmp,usr/bin,usr/local/bin} "$out"
    for applet in $(${pkgsStatic.busybox}/bin/busybox --list); do
      ln -s busybox "$root/bin/$applet"
    done
    cp ${pkgsStatic.busybox}/bin/busybox "$root/bin/busybox"
    # busybox's setpriv cannot change uid; the Amp image has util-linux's here.
    cp ${pkgsStatic.util-linux}/bin/setpriv "$root/usr/bin/setpriv"
    install -m 0755 ${jorb-init}/bin/jorb-init "$root/jorb-init"
    install -m 0755 ${./amp.sh} "$root/usr/local/bin/amp"
    printf '%s\n' 'root:x:0:0::/root:/bin/sh' 'user:x:1000:1000::/home/user:/bin/sh' >"$root/etc/passwd"
    printf '%s\n' 'root:x:0:' 'user:x:1000:' >"$root/etc/group"
    echo 'PATH=/usr/local/bin:/usr/bin:/bin' >"$root/etc/jorb-env"

    truncate -s 512M "$out/test.raw"
    mkfs.ext4 -q -F -d "$root" "$out/test.raw"
  '';
}
