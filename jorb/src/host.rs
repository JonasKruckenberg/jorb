//! The host jorb runs on: what it is, where it keeps its files, which slot is
//! free, how its units are named, and how to run a program on it.
//!
//! `Host` is the answer to "which host, with what sizing" and it owns every
//! path jorb touches, so nothing else has to know the layout. What Amp asked
//! for lives in `request.rs`; what to do to one orb lives in `orb.rs`.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use clap::Args;
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

use crate::fixed;
use crate::request::OrbId;

/// This host, as the Nix module describes it: how many orbs it will run, how
/// big each one is, and where its files live.
///
/// Every value comes from a `JORB_*` variable the root-side wrapper sets, so
/// none of it is the caller's to choose. See Security in `AGENTS.md`.
#[derive(Debug, Args)]
pub struct Host {
    /// How many orbs may run at once. Each one holds a slot.
    #[arg(long, env = "JORB_SLOTS", default_value = "1")]
    slots: std::num::NonZeroU16,

    #[arg(long, env = "JORB_VCPUS", default_value = "4")]
    pub vcpus: std::num::NonZeroU8,

    #[arg(long, env = "JORB_MEMORY_MIB", default_value = "6144")]
    pub memory_mib: std::num::NonZeroU32,

    /// The guest kernel, which `nix build .#guest-kernel` produces.
    #[arg(long, env = "JORB_KERNEL")]
    pub kernel: PathBuf,

    /// Where the base image and the orbs' disks live. XFS, for reflinks.
    #[arg(long, env = "JORB_STATE_DIR", default_value = fixed::STATE_DIR)]
    state_dir: PathBuf,

    /// Where the config disks and API sockets live. Cleared on boot.
    #[arg(long, env = "JORB_RUN_DIR", default_value = fixed::RUN_DIR)]
    run_dir: PathBuf,

    /// The VMM to run. A setting so the bounds tests can point it at a
    /// program that never opens its socket.
    #[arg(long, env = "JORB_CH_BIN", default_value = fixed::CLOUD_HYPERVISOR)]
    pub cloud_hypervisor: PathBuf,
}

impl Host {
    /// The ext4 image every orb's disk is cloned from.
    pub fn base_image(&self) -> PathBuf {
        self.state_dir.join(fixed::BASE_IMAGE)
    }

    /// The one lock, held for the whole of every job.
    pub fn lock(&self) -> PathBuf {
        self.state_dir.join(fixed::LOCK)
    }

    /// The directory the orbs' disks live in. The disk and the partial disk
    /// share it, so the rename between them is atomic.
    pub fn disks_directory(&self) -> PathBuf {
        self.state_dir.join(fixed::DISKS_DIRECTORY)
    }

    /// Where a clone is written before it becomes an orb's disk. One name for
    /// the whole host, because one job runs at a time.
    pub fn partial_disk(&self) -> PathBuf {
        self.disks_directory().join(fixed::PARTIAL_DISK)
    }

    /// The tmpfs holding the config disks and API sockets, cleared on boot.
    pub fn run_directory(&self) -> &Path {
        &self.run_dir
    }

    /// Where the base image and the orbs' disks live.
    pub fn state_directory(&self) -> &Path {
        &self.state_dir
    }

    /// How many slots this host has: what it was configured for, bounded by
    /// what a slot's addressing can express.
    pub fn slot_count(&self) -> u16 {
        self.slots.get().min(fixed::MAX_SLOTS)
    }

    pub fn disk(&self, orb: &OrbId) -> PathBuf {
        self.disks_directory().join(format!("{orb}.raw"))
    }

    pub fn config_disk(&self, orb: &OrbId) -> PathBuf {
        self.run_dir.join(format!("{orb}.cfg"))
    }

    pub fn api_socket(&self, orb: &OrbId) -> PathBuf {
        self.run_dir.join(format!("{orb}.sock"))
    }

    /// The lowest slot whose tap is absent.
    ///
    /// The tap is the only record of a slot being taken: Cloud Hypervisor
    /// creates it on boot and it goes away with the process, so there is no
    /// counter to get out of step with reality. No free slot is the host's
    /// capacity error, and the slot count is private so that nothing else can
    /// invent its own way of asking.
    pub fn lowest_free_slot(&self) -> Result<Slot> {
        let slots = usize::from(self.slot_count());

        (0..=u8::MAX)
            .take(slots)
            .map(Slot::new)
            .find(|slot| {
                !Path::new(fixed::SYS_CLASS_NET)
                    .join(slot.tap_name())
                    .exists()
            })
            .with_context(|| format!("no free slot: all {slots} are taken"))
    }
}

/// A slot on the host. One orb occupies one slot, and the slot decides the
/// orb's tap, its addresses and what its guest is told at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot(u8);

impl Slot {
    /// Slots are `0..JORB_SLOTS`, at most [`fixed::MAX_SLOTS`], so the index
    /// is a byte.
    pub fn new(index: u8) -> Self {
        Self(index)
    }

    pub fn index(self) -> u8 {
        self.0
    }

    /// The host-side interface Cloud Hypervisor creates for this slot. A slot
    /// is free when its tap is absent.
    pub fn tap_name(self) -> String {
        format!("{}{}", fixed::TAP_PREFIX, self.0)
    }

    /// The host's end of the point-to-point link, `10.77.S.1`.
    pub fn host_address(self) -> Ipv4Addr {
        Ipv4Addr::new(10, 77, self.0, 1)
    }

    /// The guest's end, `10.77.S.2`.
    pub fn guest_address(self) -> Ipv4Addr {
        Ipv4Addr::new(10, 77, self.0, 2)
    }

    /// What the guest in this slot is told at boot.
    ///
    /// The console is the architecture's serial port, the root disk is the
    /// first disk, PID 1 is jorb-init, and the network comes from the kernel's
    /// `ip=`, so nothing inside the guest configures an interface.
    pub fn cmdline(self, orb: &OrbId) -> String {
        format!(
            "console={console} root=/dev/vda rw init={init} \
             ip={guest}::{host}:{netmask}:{orb}:{interface}:off:{primary_dns}:{secondary_dns}",
            console = fixed::CONSOLE_DEVICE,
            init = fixed::GUEST_INIT,
            guest = self.guest_address(),
            host = self.host_address(),
            netmask = fixed::GUEST_NETMASK,
            interface = fixed::GUEST_INTERFACE,
            primary_dns = fixed::PRIMARY_DNS,
            secondary_dns = fixed::SECONDARY_DNS,
        )
    }
}

/// The unit an orb runs as.
pub fn unit_name(orb: &OrbId) -> String {
    format!("{}{orb}{}", fixed::UNIT_PREFIX, fixed::UNIT_SUFFIX)
}

/// The orb a unit belongs to, or `None` when the name is not one of ours.
///
/// The inverse of [`unit_name`]. The id goes back through the same parser as
/// the one Amp sent, so a unit name cannot smuggle in an id `OrbId` refuses.
pub fn orb_of_unit(unit: &str) -> Option<OrbId> {
    unit.strip_prefix(fixed::UNIT_PREFIX)
        .and_then(|rest| rest.strip_suffix(fixed::UNIT_SUFFIX))
        .and_then(|id| id.parse().ok())
}

/// The orb units systemd knows about on this host, bounded by the number of
/// slots a host can have.
///
/// Only an orb's unit matches the pattern, so the provider's own unit and the
/// gates' `jorb-gate-*` units are out of reach of anything built on this.
pub async fn orb_units() -> Result<Vec<String>> {
    let pattern = format!("{}*{}", fixed::UNIT_PREFIX, fixed::UNIT_SUFFIX);
    let listed = run(
        fixed::SYSTEMCTL,
        &[
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            "--no-pager",
            &pattern,
        ],
    )
    .await?;

    Ok(listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.ends_with(fixed::UNIT_SUFFIX))
        .take(fixed::MAX_UNITS_TO_RECOVER)
        .map(str::to_owned)
        .collect())
}

/// systemd's view of a unit. `show` also answers for a unit it has never heard
/// of, calling it inactive, so a unit `--collect` has already taken away needs
/// no special case.
pub async fn unit_is_active(unit: &str) -> Result<bool> {
    let state = run(
        fixed::SYSTEMCTL,
        &["show", "--property=ActiveState", "--value", unit],
    )
    .await?;

    Ok(matches!(
        state.trim(),
        "active" | "activating" | "deactivating" | "reloading"
    ))
}

/// Runs a program on this host and gives back its stdout, failing if it does.
///
/// Arguments are passed as arguments: nothing jorb runs goes through a shell,
/// because `ORB_ID` reaches some of them. Every caller wants a program that
/// succeeded, so there is one way to run one.
pub async fn run(program: &str, arguments: &[&str]) -> Result<String> {
    debug!(program, ?arguments, "running");

    let output = timeout(
        fixed::SUBPROCESS_TIMEOUT,
        Command::new(program).args(arguments).output(),
    )
    .await
    .with_context(|| {
        format!(
            "{program} did not finish within {:?}",
            fixed::SUBPROCESS_TIMEOUT
        )
    })?
    .with_context(|| format!("running {program}"))?;

    ensure!(
        output.status.success(),
        "{program} exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// `Host` is flattened into the real parser, so a test needs one of its
    /// own to parse it alone.
    #[derive(Parser)]
    struct Only {
        #[command(flatten)]
        host: Host,
    }

    fn test_host() -> Host {
        Only::try_parse_from(["jorb", "--kernel", "/var/lib/jorb/kernel"])
            .expect("the host parses")
            .host
    }

    fn orb(raw: &str) -> OrbId {
        raw.parse().expect("the test's id is valid")
    }

    #[test]
    fn the_host_describes_the_v1_machine_by_default() {
        let host = test_host();

        assert_eq!(host.slots.get(), 1);
        assert_eq!(host.vcpus.get(), 4);
        assert_eq!(host.memory_mib.get(), 6144);
        assert_eq!(host.state_dir, Path::new(fixed::STATE_DIR));
        assert_eq!(host.run_directory(), Path::new(fixed::RUN_DIR));
    }

    #[test]
    fn an_orbs_paths_follow_its_id() {
        let host = test_host();
        let id = orb("amp-1");

        assert_eq!(host.base_image(), Path::new("/var/lib/jorb/base.raw"));
        assert_eq!(host.lock(), Path::new("/var/lib/jorb/lock"));
        assert_eq!(host.disks_directory(), Path::new("/var/lib/jorb/disks"));
        assert_eq!(host.partial_disk(), Path::new("/var/lib/jorb/disks/.tmp"));
        assert_eq!(host.disk(&id), Path::new("/var/lib/jorb/disks/amp-1.raw"));
        assert_eq!(host.config_disk(&id), Path::new("/run/jorb/amp-1.cfg"));
        assert_eq!(host.api_socket(&id), Path::new("/run/jorb/amp-1.sock"));
    }

    #[test]
    fn a_slots_tap_and_addresses_follow_its_index() {
        for (index, tap, host_address, guest) in [
            (0, "jorb0", [10, 77, 0, 1], [10, 77, 0, 2]),
            (255, "jorb255", [10, 77, 255, 1], [10, 77, 255, 2]),
        ] {
            let slot = Slot::new(index);

            assert_eq!(slot.tap_name(), tap);
            assert_eq!(slot.host_address(), Ipv4Addr::from(host_address));
            assert_eq!(slot.guest_address(), Ipv4Addr::from(guest));
        }
    }

    #[test]
    fn the_cmdline_is_the_fixed_interface() {
        assert_eq!(
            Slot::new(0).cmdline(&orb("amp-T-1a2b")),
            format!(
                "console={} root=/dev/vda rw init=/jorb-init \
                 ip=10.77.0.2::10.77.0.1:255.255.255.252:amp-T-1a2b:eth0:off:1.1.1.1:9.9.9.9",
                fixed::CONSOLE_DEVICE
            )
        );
    }

    /// Slot 0's addresses are also what a hardcoded zero would produce, so the
    /// case above cannot tell whether the slot is used at all.
    #[test]
    fn the_cmdline_follows_the_slot() {
        assert!(
            Slot::new(7)
                .cmdline(&orb("amp-1"))
                .contains("ip=10.77.7.2::10.77.7.1:255.255.255.252:amp-1:eth0:")
        );
    }

    #[test]
    fn a_unit_name_and_the_orb_it_names_are_inverses() {
        let id = orb("amp-T-1a2b");

        assert_eq!(unit_name(&id), "jorb-orb-amp-T-1a2b.service");
        assert_eq!(orb_of_unit(&unit_name(&id)), Some(id));
    }

    /// The `jorb-orb-` prefix keeps the provider's own unit, the gates' units
    /// and everything else on the host out of recover's reach.
    #[test]
    fn a_unit_that_is_not_ours_belongs_to_no_orb() {
        for unit in [
            "sshd.service",
            "jorb-provider.service",
            "jorb-gate-g4.service",
            "jorb-amp-1.service",
            "jorb-orb-.service",
            "jorb-orb-amp-1.slice",
            "jorb-orb-../x.service",
            "jorb-orb-a b.service",
        ] {
            assert!(
                orb_of_unit(unit).is_none(),
                "{unit} should not parse as an orb's unit"
            );
        }
    }
}
