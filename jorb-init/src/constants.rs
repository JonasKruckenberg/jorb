//! Every constant the guest side uses: what it reads, who it runs as, and the
//! two bounds it imposes. Each carries the reason for its value.

use std::ffi::CStr;

/// Where the config disk arrives: the second disk in attach order,
/// read-only.
pub const CONFIG_DISK_DEVICE: &str = "/dev/vdb";

/// Where the kernel keeps the hostname `ip=` set, and the file that makes it
/// resolve.
pub const HOSTNAME: &str = "/proc/sys/kernel/hostname";
pub const HOSTS: &str = "/etc/hosts";

/// What `/etc/hosts` says before the hostname's own line.
pub const LOOPBACK_HOSTS: &str =
    "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n";

/// The hostname's address: a loopback address of its own keeps it off the
/// `localhost` line, as Debian does.
pub const HOSTNAME_ADDRESS: &str = "127.0.0.2";

/// The image's own environment, written into the base image by
/// jorb-prepare-image from the container config's `Env`.
pub const ENV_FILE: &str = "/etc/jorb-env";

/// How long a dirty page may sit unwritten, in centiseconds.
///
/// Pause is a power cut, so this is the size of the window in which the
/// guest's work can be lost. One second loses little and does not write
/// constantly.
pub const DIRTY_WRITEBACK_CENTISECS: u32 = 100;

/// Size of the guest's /dev/shm, as the mount option itself: enough for the
/// toolchains an orb runs, and small enough that it cannot quietly become the
/// whole guest's memory.
pub const DEV_SHM_OPTIONS: &CStr = c"size=1G";

/// Who bootstrap runs as, and where. Both come from the Amp orb image.
pub const GUEST_USER: &str = "user";
pub const GUEST_HOME: &str = "/home/user";

/// What drops privileges, and what it then runs. Both are the image's, not
/// jorb's, and both are absolute so neither depends on the PATH the image
/// happens to set.
pub const SETPRIV: &str = "/usr/bin/setpriv";
pub const AMP: &str = "/usr/local/bin/amp";
