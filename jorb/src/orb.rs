//! One orb's shape on this host: which slot it occupies, what its addresses
//! are, where its files live, and what the guest is told at boot.
//!
//! All of it is derived from a slot number and an orb id, so none of these
//! functions touch the filesystem or the network. The module refuses to know
//! how a slot was chosen or whether any of these paths exist.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use secrecy::{ExposeSecret, SecretString};

use crate::job::{OrbId, Settings};
use crate::limits;

/// A slot on the host. One orb occupies one slot, and the slot decides the
/// orb's tap and addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Slot(u8);

impl Slot {
    /// Slots are `0..JORB_SLOTS`, at most [`limits::MAX_SLOTS`], which is why
    /// the index is a byte.
    pub fn new(index: u8) -> Self {
        Self(index)
    }

    pub fn index(self) -> u8 {
        self.0
    }

    /// The host-side interface Cloud Hypervisor creates for this slot. Its
    /// absence is what makes a slot free.
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

    /// A /30: the two addresses above and nothing else.
    pub fn netmask(self) -> Ipv4Addr {
        Ipv4Addr::new(255, 255, 255, 252)
    }
}

/// Where this orb's files are. Paths only: nothing here says they exist.
#[derive(Debug, Clone)]
pub struct Paths {
    pub base_image: PathBuf,
    pub disk: PathBuf,
    /// The disk is written here first, then renamed, so a kill mid-copy
    /// leaves something the next job overwrites rather than a short disk.
    pub partial_disk: PathBuf,
    pub config_disk: PathBuf,
    pub api_socket: PathBuf,
    pub lock: PathBuf,
}

impl Paths {
    pub fn new(settings: &Settings, orb: &OrbId) -> Self {
        let disks = settings.state_dir.join(limits::DISKS_DIRECTORY);

        Self {
            base_image: settings.state_dir.join(limits::BASE_IMAGE),
            disk: disks.join(format!("{orb}.raw")),
            partial_disk: disks.join(limits::PARTIAL_DISK),
            config_disk: settings.run_dir.join(format!("{orb}.cfg")),
            api_socket: settings.run_dir.join(format!("{orb}.sock")),
            lock: settings.state_dir.join(limits::LOCK),
        }
    }

    /// The directory the disk and the partial disk share, which is what makes
    /// the rename between them atomic.
    pub fn disks_directory(&self) -> &Path {
        self.partial_disk
            .parent()
            .unwrap_or(Path::new(limits::STATE_DIR))
    }
}

/// The unit name for an orb. Units are `jorb-<id>.service` in `jorb.slice`.
pub fn unit_name(orb: &OrbId) -> String {
    format!("{}{orb}.service", limits::UNIT_PREFIX)
}

/// What the guest is told at boot.
///
/// The console is the architecture's serial port, the root disk is the first
/// disk, PID 1 is jorb-init, and the network comes from the kernel's `ip=` so
/// that nothing inside the guest has to configure an interface.
pub fn cmdline(slot: Slot, orb: &OrbId) -> String {
    format!(
        "console={console} root=/dev/vda rw init={init} \
         ip={guest}::{host}:{netmask}:{orb}:{interface}:off:{primary_dns}:{secondary_dns}",
        console = limits::CONSOLE_DEVICE,
        init = limits::GUEST_INIT,
        guest = slot.guest_address(),
        host = slot.host_address(),
        netmask = slot.netmask(),
        interface = limits::GUEST_INTERFACE,
        primary_dns = limits::PRIMARY_DNS,
        secondary_dns = limits::SECONDARY_DNS,
    )
}

/// The config disk: the only thing the host tells one particular orb.
///
/// Values are refused if they contain a newline or a NUL, because either
/// would let one value pretend to be two, or end the disk early.
#[derive(Debug)]
pub struct ConfigDisk {
    token: SecretString,
    amp_url: Option<String>,
}

impl ConfigDisk {
    /// Refuses a value the disk could not carry back out unchanged. The
    /// offending value is never in the message: one of the two is the token.
    pub fn new(token: Option<&SecretString>, amp_url: Option<&str>) -> Result<Self> {
        let token = token.context("spawn needs ORB_TOKEN, and this job has none")?;

        ensure!(
            !carries_a_separator(token.expose_secret()),
            "the value of ORB_TOKEN contains a newline or a NUL, which a config disk cannot carry"
        );

        if let Some(url) = amp_url {
            ensure!(
                !carries_a_separator(url),
                "the value of AMP_URL contains a newline or a NUL, which a config disk cannot carry"
            );
        }

        Ok(Self {
            token: SecretString::from(token.expose_secret()),
            amp_url: amp_url.map(str::to_owned),
        })
    }

    /// The disk as the guest reads it: `KEY=VALUE` lines, NUL-padded to a
    /// fixed size so that two disks are byte-equal exactly when they say the
    /// same thing.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut contents = format!("ORB_TOKEN={}\n", self.token.expose_secret());

        if let Some(url) = &self.amp_url {
            contents.push_str(&format!("AMP_URL={url}\n"));
        }

        ensure!(
            contents.len() <= limits::CONFIG_DISK_BYTES,
            "the config disk needs {} bytes, which is more than the {} it has",
            contents.len(),
            limits::CONFIG_DISK_BYTES
        );

        let mut disk = contents.into_bytes();
        disk.resize(limits::CONFIG_DISK_BYTES, 0);

        Ok(disk)
    }
}

/// A newline would make one value look like two; a NUL would end the disk
/// where the padding starts.
fn carries_a_separator(value: &str) -> bool {
    value.contains('\n') || value.contains('\0')
}

#[cfg(test)]
mod tests {
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
    fn slot_zero_takes_the_first_network() {
        let slot = Slot::new(0);

        assert_eq!(slot.tap_name(), "jorb0");
        assert_eq!(slot.host_address(), Ipv4Addr::new(10, 77, 0, 1));
        assert_eq!(slot.guest_address(), Ipv4Addr::new(10, 77, 0, 2));
        assert_eq!(slot.netmask(), Ipv4Addr::new(255, 255, 255, 252));
    }

    #[test]
    fn the_last_slot_takes_the_last_network() {
        let slot = Slot::new(255);

        assert_eq!(slot.tap_name(), "jorb255");
        assert_eq!(slot.host_address(), Ipv4Addr::new(10, 77, 255, 1));
        assert_eq!(slot.guest_address(), Ipv4Addr::new(10, 77, 255, 2));
    }

    #[test]
    fn every_slot_has_its_own_network() {
        let networks: std::collections::BTreeSet<_> = (0..=255)
            .map(|index| Slot::new(index).host_address())
            .collect();

        assert_eq!(networks.len(), 256);
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

    #[test]
    fn the_cmdline_follows_the_slot() {
        let cmdline = cmdline(Slot::new(7), &orb("amp-1"));

        assert!(cmdline.contains("ip=10.77.7.2::10.77.7.1:255.255.255.252:amp-1:eth0:"));
    }

    #[test]
    fn a_unit_is_named_after_its_orb() {
        assert_eq!(unit_name(&orb("amp-1")), "jorb-amp-1.service");
    }

    #[test]
    fn an_orbs_paths_follow_its_id() {
        let paths = Paths::new(&test_settings(), &orb("amp-1"));

        assert_eq!(paths.base_image, PathBuf::from("/var/lib/jorb/base.raw"));
        assert_eq!(paths.disk, PathBuf::from("/var/lib/jorb/disks/amp-1.raw"));
        assert_eq!(
            paths.partial_disk,
            PathBuf::from("/var/lib/jorb/disks/.tmp")
        );
        assert_eq!(paths.config_disk, PathBuf::from("/run/jorb/amp-1.cfg"));
        assert_eq!(paths.api_socket, PathBuf::from("/run/jorb/amp-1.sock"));
        assert_eq!(paths.lock, PathBuf::from("/var/lib/jorb/lock"));
        assert_eq!(paths.disks_directory(), Path::new("/var/lib/jorb/disks"));
    }

    #[test]
    fn a_config_disk_is_padded_to_its_fixed_size() {
        let disk = ConfigDisk::new(Some(&"a-token".into()), None)
            .expect("the config disk is representable")
            .encode()
            .expect("the config disk fits");

        assert_eq!(disk.len(), limits::CONFIG_DISK_BYTES);
        assert_eq!(&disk[..18], b"ORB_TOKEN=a-token\n");
        assert!(disk[18..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn the_amp_url_is_only_there_when_it_was_given() {
        let without = ConfigDisk::new(Some(&"a-token".into()), None)
            .unwrap()
            .encode()
            .unwrap();
        let with = ConfigDisk::new(Some(&"a-token".into()), Some("https://amp.example"))
            .unwrap()
            .encode()
            .unwrap();

        assert!(!String::from_utf8_lossy(&without).contains("AMP_URL"));
        assert!(String::from_utf8_lossy(&with).contains("AMP_URL=https://amp.example\n"));
    }

    #[test]
    fn two_config_disks_with_the_same_contents_are_byte_equal() {
        let first = ConfigDisk::new(Some(&"a-token".into()), None)
            .unwrap()
            .encode()
            .unwrap();
        let second = ConfigDisk::new(Some(&"a-token".into()), None)
            .unwrap()
            .encode()
            .unwrap();
        let different = ConfigDisk::new(Some(&"another-token".into()), None)
            .unwrap()
            .encode()
            .unwrap();

        assert_eq!(first, second);
        assert_ne!(first, different);
    }

    #[test]
    fn spawn_without_a_token_is_refused() {
        let refusal = ConfigDisk::new(None, None)
            .expect_err("a spawn without a token has nothing to write")
            .to_string();

        assert!(refusal.contains("ORB_TOKEN"), "{refusal}");
    }

    #[test]
    fn a_token_with_a_newline_or_a_nul_is_refused() {
        for token in ["two\nlines", "a\0b", "trailing\n"] {
            let refusal = ConfigDisk::new(Some(&token.into()), None)
                .expect_err("this token is not representable")
                .to_string();

            assert!(refusal.contains("ORB_TOKEN"), "{token:?}: {refusal}");
            assert!(!refusal.contains(token), "the token leaked: {refusal}");
        }
    }

    #[test]
    fn an_amp_url_with_a_newline_is_refused() {
        let refusal = ConfigDisk::new(Some(&"a-token".into()), Some("https://a\nb"))
            .expect_err("this url is not representable")
            .to_string();

        assert!(refusal.contains("AMP_URL"), "{refusal}");
    }

    #[test]
    fn a_config_that_does_not_fit_is_refused() {
        let token = "a".repeat(limits::CONFIG_DISK_BYTES);

        let refusal = ConfigDisk::new(Some(&token.into()), None)
            .unwrap()
            .encode()
            .expect_err("a config that does not fit is refused")
            .to_string();

        assert!(refusal.contains("more than the 4096 it has"), "{refusal}");
    }

    #[test]
    fn the_token_does_not_appear_in_a_config_disks_debug_output() {
        let disk = ConfigDisk::new(Some(&"a-token-that-must-not-leak".into()), None).unwrap();

        let printed = format!("{disk:?}");

        assert!(
            !printed.contains("a-token-that-must-not-leak"),
            "the token leaked into Debug: {printed}"
        );
    }
}
