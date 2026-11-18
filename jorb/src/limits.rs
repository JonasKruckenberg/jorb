//! The bounds that keep jorb's loops and waits finite, and the sizes the
//! design fixes. Each says why it has its value. Names, paths and programs
//! live in the module that uses them.

use std::num::{NonZeroU8, NonZeroU32};
use std::time::Duration;

/// The longest orb id. The real bound is the hostname it becomes, and a Linux
/// hostname is 64 bytes.
pub const MAX_ORB_ID_LEN: usize = 64;

/// How many slots a host may have. One slot is one `10.77.S.0/30`, and S is a
/// byte of the address.
pub const MAX_SLOTS: u16 = 256;

/// How many orb units recover will look at: one per slot is the real number,
/// and this bounds the loop even if systemd lists something unexpected.
pub const MAX_UNITS_TO_RECOVER: usize = MAX_SLOTS as usize;

/// The v1 orb size, for a host that does not set `JORB_VCPUS` or
/// `JORB_MEMORY_MIB`.
pub const DEFAULT_VCPUS: NonZeroU8 = NonZeroU8::new(4).unwrap();
pub const DEFAULT_MEMORY_MIB: NonZeroU32 = NonZeroU32::new(6144).unwrap();

/// How long Amp allows one job before it gives up and retries.
pub const AMP_JOB_TIMEOUT: Duration = Duration::from_secs(300);

/// How long the whole job may take. jorb has to fail before Amp gives up: a
/// job still running when Amp retries would be a second job against the same
/// orb.
pub const JOB_DEADLINE: Duration = Duration::from_secs(240);

const _: () = assert!(JOB_DEADLINE.as_secs() < AMP_JOB_TIMEOUT.as_secs());

/// How long to wait for another job to let go of the lock, and how often to
/// try.
pub const LOCK_WAIT: Duration = Duration::from_secs(60);
pub const LOCK_POLL: Duration = Duration::from_millis(200);

/// How long to wait for Cloud Hypervisor to open its API socket, and how often
/// to look. It takes milliseconds on a host that is not thrashing; ten seconds
/// survives a loaded one and leaves the job inside its deadline.
pub const API_SOCKET_WAIT: Duration = Duration::from_secs(10);
pub const API_SOCKET_POLL: Duration = Duration::from_millis(50);

/// What `systemd-run` allows a VM for a clean stop. A guest is switched off
/// rather than shut down, so this is only the time systemd needs to kill the
/// VMM and reap it.
pub const UNIT_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for a stopped unit to actually be gone, and how often to
/// look. Stopping is a kill and a reap, so this is short.
pub const UNIT_GONE_WAIT: Duration = Duration::from_secs(15);
pub const UNIT_GONE_POLL: Duration = Duration::from_millis(100);

/// How long any one subprocess may take. `systemctl` and `systemd-run` talk to
/// a local bus; anything slower is a systemd that is not answering.
pub const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(30);
