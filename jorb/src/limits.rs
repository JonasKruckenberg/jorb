//! Every bound, size, duration and fixed path jorb uses, with the reason for
//! its value. Nothing elsewhere in the crate writes a bare number.

use std::net::Ipv4Addr;
use std::time::Duration;

/// The longest orb id. The regex in AGENTS.md allows 64 characters, and the
/// id has to fit a unit name, a file name and a hostname, which is the real
/// bound: a Linux hostname is 64 bytes.
pub const MAX_ORB_ID_LEN: usize = 64;

/// How many slots a host may have. One slot is one `10.77.S.0/30`, and S is a
/// byte of the address.
pub const MAX_SLOTS: usize = 256;

/// The config disk's size, which both ends agree on: jorb pads to it and
/// jorb-init reads it. One block holds the two keys v1 has many times over,
/// and a fixed size is what makes "identical config" a byte comparison.
pub const CONFIG_DISK_BYTES: usize = 4096;

/// Where the base image and the orbs' disks live. XFS, so that each disk is a
/// reflink clone rather than a copy.
pub const STATE_DIR: &str = "/var/lib/jorb";

/// Where the config disks and API sockets live: a tmpfs, so a host reboot
/// clears them.
pub const RUN_DIR: &str = "/run/jorb";

pub const BASE_IMAGE: &str = "base.raw";
pub const DISKS_DIRECTORY: &str = "disks";
pub const PARTIAL_DISK: &str = ".tmp";
pub const LOCK: &str = "lock";

/// The VMM, by name, so the host's PATH decides which one. The bounds test
/// points this at something else.
pub const CLOUD_HYPERVISOR: &str = "cloud-hypervisor";

/// Orb units are `jorb-orb-<id>.service`, and these two are the whole naming
/// rule: `host::unit_name` builds one, `host::orb_of_unit` reads one back and
/// `host::unit_pattern` is what recover lists.
///
/// The prefix is `jorb-orb-` rather than `jorb-` so that no other unit on the
/// host can be mistaken for an orb's. `jorb-provider.service` is the one that
/// used to be: it matched `jorb-*.service` and parsed as an orb called
/// "provider".
pub const UNIT_PREFIX: &str = "jorb-orb-";
pub const UNIT_SUFFIX: &str = ".service";

/// The slice every orb runs in, so one `MemoryHigh` covers all of them.
pub const SLICE: &str = "jorb.slice";

/// Taps are `jorb<S>`, and a slot is free when its tap is absent.
pub const TAP_PREFIX: &str = "jorb";

/// PID 1 in the guest, installed into the base image by jorb-prepare-image.
pub const GUEST_INIT: &str = "/jorb-init";

/// The guest's one interface, configured by the kernel from `ip=`.
pub const GUEST_INTERFACE: &str = "eth0";

/// A slot's network is a /30: the host's address, the guest's, and nothing
/// else. It is the same mask for every slot.
pub const GUEST_NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);

/// The guest's serial console, which carries its output to the host's
/// journal. The two architectures name the same thing differently.
//
// ASSUMES A3: what the guest writes to this port reaches the unit's journal.
// G4 checks it.
#[cfg(target_arch = "aarch64")]
pub const CONSOLE_DEVICE: &str = "ttyAMA0";
#[cfg(target_arch = "x86_64")]
pub const CONSOLE_DEVICE: &str = "ttyS0";

/// Public resolvers, because orbs may not talk to anything on the host's
/// network, including its resolver.
pub const PRIMARY_DNS: &str = "1.1.1.1";
pub const SECONDARY_DNS: &str = "9.9.9.9";

/// How long Amp allows one job before it gives up and retries.
pub const AMP_JOB_TIMEOUT: Duration = Duration::from_secs(300);

/// How long the whole job may take.
///
/// jorb has to fail before Amp gives up: a job still running when Amp retries
/// would be a second job against the same orb. The assertion below is what
/// keeps the two numbers in that order.
pub const JOB_DEADLINE: Duration = Duration::from_secs(240);

const _: () = assert!(JOB_DEADLINE.as_secs() < AMP_JOB_TIMEOUT.as_secs());

/// How long to wait for Cloud Hypervisor to open its API socket.
///
/// It forks, opens the socket and waits; on a host that is not thrashing this
/// takes milliseconds. Ten seconds is slow enough to survive a loaded host
/// and fast enough to leave the rest of the job inside its deadline.
pub const API_SOCKET_WAIT: Duration = Duration::from_secs(10);

/// How often to look for the socket while waiting.
pub const API_SOCKET_POLL: Duration = Duration::from_millis(50);

/// What `systemd-run` is told to allow a VM for a clean stop, and how long
/// jorb then waits for `systemctl stop` to return.
///
/// A guest is switched off rather than shut down, so this is only the time
/// systemd needs to kill the VMM and reap it.
pub const UNIT_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long any one subprocess may take. `systemctl` and `systemd-run` talk
/// to a local bus; anything slower is a systemd that is not answering.
pub const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(30);

/// How many orb units recover will look at. One per slot is the real number;
/// this bounds the loop even if systemd lists something unexpected.
pub const MAX_UNITS_TO_RECOVER: usize = MAX_SLOTS;

/// How long to wait for a stopped unit to actually be gone, and how often to
/// look. Stopping is a kill and a reap, so this is short.
pub const UNIT_GONE_WAIT: Duration = Duration::from_secs(15);
pub const UNIT_GONE_POLL: Duration = Duration::from_millis(100);

/// How long to wait for another job to let go of the lock, and how often to
/// try.
///
/// The other holder has the same job deadline, so a lock still held after
/// this long belongs to a job that is stuck rather than busy; failing then
/// lets Amp retry rather than piling jobs up behind a wedged one.
pub const LOCK_WAIT: Duration = Duration::from_secs(60);
pub const LOCK_POLL: Duration = Duration::from_millis(200);

/// Where the kernel lists interfaces. A slot's tap being here is what makes
/// the slot taken.
pub const SYS_CLASS_NET: &str = "/sys/class/net";

/// The two systemd commands jorb runs, by name, so the unit's PATH decides
/// which systemd it is talking to.
pub const SYSTEMCTL: &str = "systemctl";
pub const SYSTEMD_RUN: &str = "systemd-run";
