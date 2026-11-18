# The guest kernel: an uncompressed arm64 Image from defconfig plus
# guest-kernel.config. Cloud Hypervisor ships no reference guest config of its
# own outside a separate, unpinned kernel fork.
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
  # The kernel sets its own compiler flags.
  hardeningDisable = [ "all" ];
  env.ARCH = "arm64";

  postPatch = "patchShebangs scripts";

  configurePhase = ''
    make defconfig
    scripts/kconfig/merge_config.sh -m .config ${./guest-kernel.config}
    make olddefconfig
    bash ${./kernel-config-check.sh} .config ${./guest-kernel.config}
  '';

  buildPhase = ''make -j"$NIX_BUILD_CORES" Image'';

  installPhase = ''
    mkdir -p "$out"
    cp arch/arm64/boot/Image "$out/kernel"
    cp .config "$out/config"
  '';

  meta.platforms = [ "aarch64-linux" ];
  meta.license = lib.licenses.gpl2Only;
}
