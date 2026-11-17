# The guest kernel: one uncompressed image per host architecture, built from
# the architecture's defconfig plus the fragments in this directory.
#
# A jorb host boots guests of its own architecture, so this never cross-builds:
# aarch64 hosts get arch/arm64/boot/Image, x86_64 hosts get the uncompressed
# vmlinux that Cloud Hypervisor enters through PVH.
#
# Cloud Hypervisor's source tree ships no reference guest config (its
# ch_defconfig lives in the cloud-hypervisor/linux fork, which would be a
# second, unpinned source), so this starts from defconfig as the plan's
# fallback says.
{
  lib,
  stdenv,
  linuxPackages,
  bc,
  bison,
  elfutils,
  flex,
  gmp,
  libmpc,
  mpfr,
  openssl,
  perl,
  python3,
  rsync,
  zstd,
}:

let
  inherit (stdenv.hostPlatform) isAarch64;

  guest =
    if isAarch64 then
      {
        arch = "arm64";
        fragment = ./guest-kernel-arm64.config;
        target = "Image";
        built = "arch/arm64/boot/Image";
      }
    else
      {
        arch = "x86";
        fragment = ./guest-kernel-x86.config;
        target = "vmlinux";
        built = "vmlinux";
      };

  fragments = [
    ./guest-kernel.config
    guest.fragment
  ];
in

stdenv.mkDerivation {
  pname = "jorb-guest-kernel";
  inherit (linuxPackages.kernel) version src;

  nativeBuildInputs = [
    bc
    bison
    elfutils
    flex
    gmp
    libmpc
    mpfr
    openssl
    perl
    python3
    rsync
    zstd
  ];

  # The kernel sets its own compiler flags; stdenv's hardening wrappers only
  # fight it.
  hardeningDisable = [ "all" ];

  env.ARCH = guest.arch;

  postPatch = ''
    patchShebangs scripts
  '';

  configurePhase = ''
    runHook preConfigure
    make defconfig
    scripts/kconfig/merge_config.sh -m .config ${lib.concatStringsSep " " fragments}
    make olddefconfig
    bash ${./kernel-config-check.sh} .config ${lib.concatStringsSep " " fragments}
    runHook postConfigure
  '';

  buildPhase = ''
    runHook preBuild
    make -j"$NIX_BUILD_CORES" ${guest.target}
    runHook postBuild
  '';

  # One name whatever the architecture, because this is what jorb passes as
  # JORB_KERNEL. The config travels with it so a gate can show what booted.
  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp ${guest.built} "$out/kernel"
    cp .config "$out/config"
    runHook postInstall
  '';

  meta = {
    description = "Guest kernel for jorb's Cloud Hypervisor microVMs";
    platforms = [
      "aarch64-linux"
      "x86_64-linux"
    ];
    license = lib.licenses.gpl2Only;
  };
}
