# The guest kernel: an uncompressed arm64 Image, built from defconfig plus
# the fragments in this directory.
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
  fragments = [
    ./guest-kernel.config
    ./guest-kernel-arm64.config
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

  env.ARCH = "arm64";

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
    make -j"$NIX_BUILD_CORES" Image
    runHook postBuild
  '';

  # `kernel` is what jorb passes as JORB_KERNEL. The config travels with it so
  # a gate can show what booted.
  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp arch/arm64/boot/Image "$out/kernel"
    cp .config "$out/config"
    runHook postInstall
  '';

  meta = {
    description = "Guest kernel for jorb's Cloud Hypervisor microVMs";
    platforms = [ "aarch64-linux" ];
    license = lib.licenses.gpl2Only;
  };
}
