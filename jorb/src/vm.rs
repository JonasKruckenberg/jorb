//! The fixed VM shape and booting it: the slot an orb runs in, the cmdline its
//! guest is told, the VMM's own command line, and the call into `jorb-vmm`.
//!
//! A slot is free when its tap is absent. Cloud Hypervisor creates the tap on
//! boot and it goes away with the process, so the tap is the only record of a
//! slot being taken.

use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use jorb_vmm::{Tap, VmShape, VmState};
use tokio::time::sleep;

use crate::disk;
use crate::job::OrbId;
use crate::limits;
use crate::settings::Settings;

/// Taps are `jorb<S>`.
const TAP_PREFIX: &str = "jorb";

/// Where the kernel lists interfaces.
const SYS_CLASS_NET: &str = "/sys/class/net";

/// A slot's network is a /30: the host's address, the guest's, nothing else.
const GUEST_NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);

/// The serial port, which carries the guest's output to the unit's journal.
const CONSOLE_DEVICE: &str = "ttyAMA0";
const GUEST_INIT: &str = "/jorb-init";
const GUEST_INTERFACE: &str = "eth0";

/// Public resolvers, because orbs may not talk to anything on the host's
/// network, its resolver included.
const PRIMARY_DNS: &str = "1.1.1.1";
const SECONDARY_DNS: &str = "9.9.9.9";

/// One orb's place on the host: its tap and its two addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot(u8);

impl Slot {
    /// The lowest free slot. No free slot is the host's capacity error, and
    /// there is no other.
    pub fn lowest_free(settings: &Settings) -> Result<Self> {
        (0..=u8::MAX)
            .take(usize::from(settings.slots))
            .map(Self)
            .find(|slot| slot.is_free())
            .with_context(|| format!("no free slot: all {} are taken", settings.slots))
    }

    pub fn index(self) -> u8 {
        self.0
    }

    fn is_free(self) -> bool {
        !Path::new(SYS_CLASS_NET).join(self.tap_name()).exists()
    }

    fn tap_name(self) -> String {
        format!("{TAP_PREFIX}{}", self.0)
    }

    /// `10.77.S.1`.
    fn host_address(self) -> Ipv4Addr {
        Ipv4Addr::new(10, 77, self.0, 1)
    }

    /// `10.77.S.2`.
    fn guest_address(self) -> Ipv4Addr {
        Ipv4Addr::new(10, 77, self.0, 2)
    }
}

/// What the guest is told at boot. The network comes from the kernel's `ip=`,
/// so nothing inside the guest configures an interface.
fn cmdline(slot: Slot, orb: &OrbId) -> String {
    format!(
        "console={CONSOLE_DEVICE} root=/dev/vda rw init={GUEST_INIT} \
         ip={guest}::{host}:{GUEST_NETMASK}:{orb}:{GUEST_INTERFACE}:off:{PRIMARY_DNS}:{SECONDARY_DNS}",
        guest = slot.guest_address(),
        host = slot.host_address(),
    )
}

/// The socket the orb's VMM listens on.
pub fn api_socket_path(settings: &Settings, orb: &OrbId) -> PathBuf {
    settings.run_directory.join(format!("{orb}.sock"))
}

/// What the orb's unit runs: the VMM, listening on the orb's API socket and
/// waiting to be given a VM.
pub fn vmm_command(settings: &Settings, orb: &OrbId) -> Vec<String> {
    vec![
        settings.cloud_hypervisor.to_string_lossy().into_owned(),
        format!("--api-socket={}", api_socket_path(settings, orb).display()),
    ]
}

/// What the VMM behind the orb's socket says about it.
pub async fn state(settings: &Settings, orb: &OrbId) -> Result<VmState> {
    Ok(jorb_vmm::state(&api_socket_path(settings, orb)).await?)
}

/// Waits for the VMM to open its socket, then creates and boots the orb's VM
/// in `slot`. Returns once the VMM reports it running.
pub async fn boot(settings: &Settings, orb: &OrbId, slot: Slot) -> Result<()> {
    let socket = api_socket_path(settings, orb);
    wait_for_socket(&socket).await?;

    let cmdline = cmdline(slot, orb);
    let tap = slot.tap_name();
    let root_disk = disk::disk_path(settings, orb);
    let config_disk = disk::config_disk_path(settings, orb);

    let shape = VmShape {
        kernel: &settings.kernel,
        cmdline: &cmdline,
        cpus: settings.vcpus,
        memory_mib: settings.memory_mib,
        root_disk: &root_disk,
        read_only_disk: &config_disk,
        tap: Tap {
            name: &tap,
            host_address: slot.host_address(),
            netmask: GUEST_NETMASK,
        },
    };

    jorb_vmm::boot(&socket, &shape)
        .await
        .context("creating and booting the VM")
}

/// Waits until something accepts a connection on the socket. The file
/// appears before the VMM listens on it, so a missing file and a refused
/// connection both mean "not yet".
async fn wait_for_socket(socket: &Path) -> Result<()> {
    let deadline = Instant::now() + limits::API_SOCKET_WAIT;

    while !accepts_connections(socket)? {
        ensure!(
            Instant::now() < deadline,
            "nothing listened on {} within {:?}",
            socket.display(),
            limits::API_SOCKET_WAIT
        );
        sleep(limits::API_SOCKET_POLL).await;
    }

    Ok(())
}

fn accepts_connections(socket: &Path) -> Result<bool> {
    match UnixStream::connect(socket) {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error).with_context(|| format!("connecting to {}", socket.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_counts_as_ready_only_once_something_listens() {
        let directory = std::env::temp_dir().join("jorb-tests").join("socket");
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("vmm.sock");
        let _ = std::fs::remove_file(&socket);

        assert!(!accepts_connections(&socket).unwrap(), "no file");

        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(accepts_connections(&socket).unwrap(), "a listener");

        drop(listener);
        assert!(socket.exists());
        assert!(
            !accepts_connections(&socket).unwrap(),
            "a file nobody listens on"
        );
    }

    #[test]
    fn a_slots_tap_and_addresses_follow_its_index() {
        for (index, tap, host_address, guest) in [
            (0, "jorb0", [10, 77, 0, 1], [10, 77, 0, 2]),
            (255, "jorb255", [10, 77, 255, 1], [10, 77, 255, 2]),
        ] {
            let slot = Slot(index);

            assert_eq!(slot.tap_name(), tap);
            assert_eq!(slot.host_address(), Ipv4Addr::from(host_address));
            assert_eq!(slot.guest_address(), Ipv4Addr::from(guest));
        }
    }

    #[test]
    fn the_cmdline_is_the_fixed_interface() {
        assert_eq!(
            cmdline(Slot(0), &"amp-T-1a2b".parse().unwrap()),
            "console=ttyAMA0 root=/dev/vda rw init=/jorb-init \
             ip=10.77.0.2::10.77.0.1:255.255.255.252:amp-T-1a2b:eth0:off:1.1.1.1:9.9.9.9"
        );
    }

    /// Slot 0's addresses are also what a hardcoded zero would produce.
    #[test]
    fn the_cmdline_follows_the_slot() {
        assert!(
            cmdline(Slot(7), &"amp-1".parse().unwrap())
                .contains("ip=10.77.7.2::10.77.7.1:255.255.255.252:amp-1:eth0:")
        );
    }
}
