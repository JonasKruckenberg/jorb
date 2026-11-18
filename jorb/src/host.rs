//! This host: what it is, where it keeps its files, and how to run a program
//! on it.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use std::num::{NonZeroU8, NonZeroU32};
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

use crate::request::OrbId;
use crate::{Environment, constants, required};

/// This host as the Nix module describes it: how many orbs it will run, how
/// big each one is, and where its files live. A description, not a handle:
/// nothing here starts or owns anything.
///
/// Every value comes from a `JORB_*` variable the root-side wrapper sets, so
/// none of it is the caller's to choose. See Security in `AGENTS.md`.
#[derive(Debug)]
pub struct HostConfig {
    /// How many orbs may run at once, `1..=MAX_SLOTS`. Each one holds a slot.
    slots: u16,

    pub vcpus: NonZeroU8,
    pub memory_mib: NonZeroU32,

    /// The guest kernel, which `nix build .#guest-kernel` produces.
    pub kernel: PathBuf,

    /// Where the base image and the orbs' disks live. XFS, for reflinks.
    state_dir: PathBuf,

    /// Where the config disks and API sockets live. Cleared on boot.
    run_dir: PathBuf,

    /// The VMM to run. A setting so the bounds tests can point it at a
    /// program that never opens its socket.
    pub cloud_hypervisor: PathBuf,
}

impl HostConfig {
    pub fn from_environment(environment: Environment) -> Result<Self> {
        let slots = setting(environment, "JORB_SLOTS", 1)?;
        ensure!(
            (1..=constants::MAX_SLOTS).contains(&slots),
            "JORB_SLOTS is {slots}, and a host has between 1 and {} slots",
            constants::MAX_SLOTS
        );

        Ok(Self {
            slots,
            vcpus: setting(environment, "JORB_VCPUS", constants::DEFAULT_VCPUS)?,
            memory_mib: setting(
                environment,
                "JORB_MEMORY_MIB",
                constants::DEFAULT_MEMORY_MIB,
            )?,
            kernel: required(environment, "JORB_KERNEL")?.into(),
            state_dir: setting(environment, "JORB_STATE_DIR", constants::STATE_DIR.into())?,
            run_dir: setting(environment, "JORB_RUN_DIR", constants::RUN_DIR.into())?,
            cloud_hypervisor: setting(
                environment,
                "JORB_CH_BIN",
                constants::CLOUD_HYPERVISOR.into(),
            )?,
        })
    }

    /// The ext4 image every orb's disk is cloned from.
    pub fn base_image(&self) -> PathBuf {
        self.state_dir.join(constants::BASE_IMAGE)
    }

    /// The one lock, held for the whole of every job.
    pub fn lock(&self) -> PathBuf {
        self.state_dir.join(constants::LOCK)
    }

    /// The directory the orbs' disks live in. The disk and the partial disk
    /// share it, so the rename between them is atomic.
    pub fn disks_directory(&self) -> PathBuf {
        self.state_dir.join(constants::DISKS_DIRECTORY)
    }

    /// Where a clone is written before it becomes an orb's disk. One name for
    /// the whole host, because one job runs at a time.
    pub fn partial_disk(&self) -> PathBuf {
        self.disks_directory().join(constants::PARTIAL_DISK)
    }

    /// The tmpfs holding the config disks and API sockets, cleared on boot.
    pub fn run_directory(&self) -> &Path {
        &self.run_dir
    }

    pub fn slot_count(&self) -> u16 {
        self.slots
    }
}

/// A setting, or its default when the variable is not set.
fn setting<T>(environment: Environment, name: &str, default: T) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match environment(name)? {
        Some(raw) => raw.parse().with_context(|| format!("{name} is {raw:?}")),
        None => Ok(default),
    }
}

/// A slot on the host. One orb occupies one slot, and the slot decides the
/// orb's tap, its addresses and what its guest is told at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot(u8);

impl Slot {
    /// Slots are `0..JORB_SLOTS`, at most [`constants::MAX_SLOTS`], so the index
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
        format!("{}{}", constants::TAP_PREFIX, self.0)
    }

    /// The host's end of the point-to-point link, `10.77.S.1`.
    pub fn host_address(self) -> Ipv4Addr {
        Ipv4Addr::new(10, 77, self.0, 1)
    }

    /// The guest's end, `10.77.S.2`.
    pub fn guest_address(self) -> Ipv4Addr {
        Ipv4Addr::new(10, 77, self.0, 2)
    }

    /// Whether this slot is free.
    ///
    /// The tap is the only record of a slot being taken: Cloud Hypervisor
    /// creates it on boot and it goes away with the process, so there is no
    /// counter to get out of step with reality.
    pub fn is_free(self) -> bool {
        !Path::new(constants::SYS_CLASS_NET)
            .join(self.tap_name())
            .exists()
    }

    /// Every slot this host has. The index is a byte, so `take` is what bounds
    /// this rather than a comparison.
    pub fn all(host: &HostConfig) -> impl Iterator<Item = Self> {
        (0..=u8::MAX)
            .take(usize::from(host.slot_count()))
            .map(Self::new)
    }

    /// The lowest free slot on a host. No free slot is the host's capacity
    /// error, and there is no other.
    pub fn lowest_free(host: &HostConfig) -> Result<Self> {
        Self::all(host)
            .find(|slot| slot.is_free())
            .with_context(|| format!("no free slot: all {} are taken", host.slot_count()))
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
            console = constants::CONSOLE_DEVICE,
            init = constants::GUEST_INIT,
            guest = self.guest_address(),
            host = self.host_address(),
            netmask = constants::GUEST_NETMASK,
            interface = constants::GUEST_INTERFACE,
            primary_dns = constants::PRIMARY_DNS,
            secondary_dns = constants::SECONDARY_DNS,
        )
    }
}

/// Runs a program on this host and gives back its stdout, failing if it does.
///
/// Arguments are passed as arguments: nothing jorb runs goes through a shell,
/// because `ORB_ID` reaches some of them. Every caller wants a program that
/// succeeded, so there is one way to run one.
pub async fn run(program: &str, arguments: &[&str]) -> Result<String> {
    debug!(program, ?arguments, "running");

    let output = timeout(
        constants::SUBPROCESS_TIMEOUT,
        Command::new(program).args(arguments).output(),
    )
    .await
    .with_context(|| {
        format!(
            "{program} did not finish within {:?}",
            constants::SUBPROCESS_TIMEOUT
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
    use super::*;
    use crate::test_environment;

    fn test_host() -> HostConfig {
        HostConfig::from_environment(&test_environment(&[(
            "JORB_KERNEL",
            "/var/lib/jorb/kernel",
        )]))
        .expect("the host parses")
    }

    fn orb(raw: &str) -> OrbId {
        raw.parse().expect("the test's id is valid")
    }

    #[test]
    fn the_host_describes_the_v1_machine_by_default() {
        let host = test_host();

        assert_eq!(host.slots, 1);
        assert_eq!(host.vcpus.get(), 4);
        assert_eq!(host.memory_mib.get(), 6144);
        assert_eq!(host.state_dir, Path::new(constants::STATE_DIR));
        assert_eq!(host.run_directory(), Path::new(constants::RUN_DIR));
    }

    #[test]
    fn a_host_outside_what_slots_can_address_is_refused() {
        for (name, value) in [
            ("JORB_SLOTS", "0"),
            ("JORB_SLOTS", "257"),
            ("JORB_VCPUS", "0"),
            ("JORB_MEMORY_MIB", "lots"),
        ] {
            let environment =
                test_environment(&[("JORB_KERNEL", "/var/lib/jorb/kernel"), (name, value)]);
            assert!(
                HostConfig::from_environment(&environment).is_err(),
                "{name}={value} should be refused"
            );
        }

        assert!(HostConfig::from_environment(&test_environment(&[])).is_err());
    }

    #[test]
    fn the_hosts_own_paths_sit_under_its_directories() {
        let host = test_host();
        assert_eq!(host.base_image(), Path::new("/var/lib/jorb/base.raw"));
        assert_eq!(host.lock(), Path::new("/var/lib/jorb/lock"));
        assert_eq!(host.disks_directory(), Path::new("/var/lib/jorb/disks"));
        assert_eq!(host.partial_disk(), Path::new("/var/lib/jorb/disks/.tmp"));
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
                constants::CONSOLE_DEVICE
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
}
