//! Every bound and fixed path this binary uses, with the reason for its value.

/// The config disk's size. One block is enough for the handful of keys the
/// host sends, and a fixed size means the guest reads the same thing whatever
/// the disk's real length is.
pub const CONFIG_DISK_BYTES: usize = 4096;

/// Where the config disk arrives. Second disk in attach order (A2), read-only.
pub const CONFIG_DISK_DEVICE: &str = "/dev/vdb";

/// The image's own environment, written into the base image by
/// jorb-prepare-image from the container config's `Env`.
pub const ENV_FILE: &str = "/etc/jorb-env";

/// How long a dirty page may sit unwritten, in centiseconds.
///
/// Pause is a power cut, so this is the size of the window in which the
/// guest's work can be lost. One second is short enough to lose little and
/// long enough not to write constantly.
pub const DIRTY_WRITEBACK_CENTISECS: u32 = 100;

/// Size of the guest's /dev/shm. Enough for the toolchains an orb runs, and
/// small enough that it cannot quietly become the whole guest's memory.
pub const DEV_SHM_SIZE: &str = "1G";

/// Who bootstrap runs as, and where. Both come from the Amp orb image.
pub const GUEST_USER: &str = "user";
pub const GUEST_HOME: &str = "/home/user";

/// What drops privileges, and what it then runs. Both are the image's, not
/// jorb's, and both are absolute so that neither depends on the PATH the
/// image happens to set.
pub const SETPRIV: &str = "/usr/bin/setpriv";
pub const AMP: &str = "/usr/local/bin/amp";
