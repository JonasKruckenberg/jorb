//! This host's shape: a slot's addresses, where files live, what an orb's unit
//! is called, and what the guest is told at boot.
//!
//! Everything here is derived from the host's settings, a slot number and an
//! orb id. Nothing touches the filesystem, the network or systemd, and nothing
//! here knows how a slot was chosen or whether any of these paths exist.

use std::net::Ipv4Addr;
use std::path::PathBuf;

use crate::job::{OrbId, Settings};
use crate::limits;

/// A slot on the host. One orb occupies one slot, and the slot decides the
/// orb's tap and addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot(u8);

impl Slot {
    /// Slots are `0..JORB_SLOTS`, at most [`limits::MAX_SLOTS`], so the index
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
        format!("{}{}", limits::TAP_PREFIX, self.0)
    }

    /// The host's end of the point-to-point link, `10.77.S.1`.
    pub fn host_address(self) -> Ipv4Addr {
        Ipv4Addr::new(10, 77, self.0, 1)
    }

    /// The guest's end, `10.77.S.2`.
    pub fn guest_address(self) -> Ipv4Addr {
        Ipv4Addr::new(10, 77, self.0, 2)
    }
}

/// The ext4 image every orb's disk is cloned from.
pub fn base_image(settings: &Settings) -> PathBuf {
    settings.state_dir.join(limits::BASE_IMAGE)
}

/// The one lock, held for the whole of every job.
pub fn lock(settings: &Settings) -> PathBuf {
    settings.state_dir.join(limits::LOCK)
}

/// The directory the orbs' disks live in. The disk and the partial disk share
/// it, which is what makes the rename between them atomic.
pub fn disks_directory(settings: &Settings) -> PathBuf {
    settings.state_dir.join(limits::DISKS_DIRECTORY)
}

/// Where a clone is written before it becomes an orb's disk. One name for the
/// whole host, because one job runs at a time.
pub fn partial_disk(settings: &Settings) -> PathBuf {
    disks_directory(settings).join(limits::PARTIAL_DISK)
}

pub fn disk(settings: &Settings, orb: &OrbId) -> PathBuf {
    disks_directory(settings).join(format!("{orb}.raw"))
}

pub fn config_disk(settings: &Settings, orb: &OrbId) -> PathBuf {
    settings.run_dir.join(format!("{orb}.cfg"))
}

pub fn api_socket(settings: &Settings, orb: &OrbId) -> PathBuf {
    settings.run_dir.join(format!("{orb}.sock"))
}

/// The unit an orb runs as.
pub fn unit_name(orb: &OrbId) -> String {
    format!("{}{orb}{}", limits::UNIT_PREFIX, limits::UNIT_SUFFIX)
}

/// What matches every orb's unit and nothing else on the host.
pub fn unit_pattern() -> String {
    format!("{}*{}", limits::UNIT_PREFIX, limits::UNIT_SUFFIX)
}

/// The orb a unit belongs to, or `None` when the name is not one of ours.
///
/// The inverse of [`unit_name`]. The id goes back through the same parser as
/// the one Amp sent, so a unit name cannot smuggle in an id `OrbId` refuses.
pub fn orb_of_unit(unit: &str) -> Option<OrbId> {
    unit.strip_prefix(limits::UNIT_PREFIX)
        .and_then(|rest| rest.strip_suffix(limits::UNIT_SUFFIX))
        .and_then(|id| id.parse().ok())
}

/// What the guest is told at boot.
///
/// The console is the architecture's serial port, the root disk is the first
/// disk, PID 1 is jorb-init, and the network comes from the kernel's `ip=`, so
/// nothing inside the guest configures an interface.
pub fn cmdline(slot: Slot, orb: &OrbId) -> String {
    format!(
        "console={console} root=/dev/vda rw init={init} \
         ip={guest}::{host}:{netmask}:{orb}:{interface}:off:{primary_dns}:{secondary_dns}",
        console = limits::CONSOLE_DEVICE,
        init = limits::GUEST_INIT,
        guest = slot.guest_address(),
        host = slot.host_address(),
        netmask = limits::GUEST_NETMASK,
        interface = limits::GUEST_INTERFACE,
        primary_dns = limits::PRIMARY_DNS,
        secondary_dns = limits::SECONDARY_DNS,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn orb(raw: &str) -> OrbId {
        raw.parse().expect("the test's id is valid")
    }

    fn test_settings() -> Settings {
        use clap::Parser;

        #[derive(Parser)]
        struct Only {
            #[command(flatten)]
            settings: Settings,
        }

        Only::try_parse_from(["jorb", "--kernel", "/var/lib/jorb/kernel"])
            .expect("the settings parse")
            .settings
    }

    #[test]
    fn a_slots_tap_and_addresses_follow_its_index() {
        for (index, tap, host, guest) in [
            (0, "jorb0", [10, 77, 0, 1], [10, 77, 0, 2]),
            (255, "jorb255", [10, 77, 255, 1], [10, 77, 255, 2]),
        ] {
            let slot = Slot::new(index);

            assert_eq!(slot.tap_name(), tap);
            assert_eq!(slot.host_address(), Ipv4Addr::from(host));
            assert_eq!(slot.guest_address(), Ipv4Addr::from(guest));
        }
    }

    #[test]
    fn the_cmdline_is_the_fixed_interface() {
        assert_eq!(
            cmdline(Slot::new(0), &orb("amp-T-1a2b")),
            format!(
                "console={} root=/dev/vda rw init=/jorb-init \
                 ip=10.77.0.2::10.77.0.1:255.255.255.252:amp-T-1a2b:eth0:off:1.1.1.1:9.9.9.9",
                limits::CONSOLE_DEVICE
            )
        );
    }

    /// Slot 0's addresses are also what a hardcoded zero would produce, so the
    /// case above cannot tell whether the slot is used at all.
    #[test]
    fn the_cmdline_follows_the_slot() {
        assert!(
            cmdline(Slot::new(7), &orb("amp-1"))
                .contains("ip=10.77.7.2::10.77.7.1:255.255.255.252:amp-1:eth0:")
        );
    }

    #[test]
    fn a_unit_name_and_the_orb_it_names_are_inverses() {
        let id = orb("amp-T-1a2b");

        assert_eq!(unit_name(&id), "jorb-orb-amp-T-1a2b.service");
        assert_eq!(orb_of_unit(&unit_name(&id)), Some(id));
        assert_eq!(unit_pattern(), "jorb-orb-*.service");
    }

    /// The `jorb-orb-` prefix keeps the provider's own unit, and everything
    /// else on the host, out of recover's reach.
    #[test]
    fn a_unit_that_is_not_ours_belongs_to_no_orb() {
        for unit in [
            "sshd.service",
            "jorb-provider.service",
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

    #[test]
    fn an_orbs_paths_follow_its_id() {
        let settings = &test_settings();
        let id = orb("amp-1");

        assert_eq!(base_image(settings), Path::new("/var/lib/jorb/base.raw"));
        assert_eq!(lock(settings), Path::new("/var/lib/jorb/lock"));
        assert_eq!(disks_directory(settings), Path::new("/var/lib/jorb/disks"));
        assert_eq!(
            partial_disk(settings),
            Path::new("/var/lib/jorb/disks/.tmp")
        );
        assert_eq!(
            disk(settings, &id),
            Path::new("/var/lib/jorb/disks/amp-1.raw")
        );
        assert_eq!(config_disk(settings, &id), Path::new("/run/jorb/amp-1.cfg"));
        assert_eq!(api_socket(settings, &id), Path::new("/run/jorb/amp-1.sock"));
    }
}
