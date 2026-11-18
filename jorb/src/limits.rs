//! The bounds that keep jorb's loops and waits finite. Names, paths and
//! programs live in the module that uses them.

use std::time::Duration;

/// A Linux hostname is 64 bytes, and an orb id becomes one.
pub const MAX_ORB_ID_LEN: usize = 64;

/// Slot S is a byte of `10.77.S.0/30`.
pub const MAX_SLOTS: u16 = 256;

/// Recover looks at no more units than there can be slots.
pub const MAX_UNITS: usize = MAX_SLOTS as usize;

/// How long Amp allows one job before it retries it.
pub const AMP_JOB_TIMEOUT: Duration = Duration::from_secs(300);

/// The lock wait comes before the job deadline, and their sum must stay under
/// Amp's timeout: a job still running when Amp retries is a second job against
/// the same orb.
pub const LOCK_WAIT: Duration = Duration::from_secs(50);
pub const LOCK_POLL: Duration = Duration::from_millis(200);
pub const JOB_DEADLINE: Duration = Duration::from_secs(230);
const _: () = assert!(LOCK_WAIT.as_secs() + JOB_DEADLINE.as_secs() < AMP_JOB_TIMEOUT.as_secs());

/// How long a started VM has to report itself running, and how often to ask.
pub const BOOT_WAIT: Duration = Duration::from_secs(10);
pub const BOOT_POLL: Duration = Duration::from_millis(100);

/// A guest is switched off, not shut down, so stopping is a kill and a reap.
pub const UNIT_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Any one subprocess. Longer than a stop that ends in a kill (twice
/// `UNIT_STOP_TIMEOUT`); anything slower is a systemd or VMM not answering.
pub const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(15);
const _: () = assert!(2 * UNIT_STOP_TIMEOUT.as_secs() < SUBPROCESS_TIMEOUT.as_secs());
