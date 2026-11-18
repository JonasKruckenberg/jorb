//! The fixed VM shape and booting it through `jorb-vmm`, plus the one question
//! jorb asks a VMM: is your VM running?
//!
//! A slot is free when its tap is absent: the VMM creates the tap when it
//! creates the VM, and the tap goes with the process.

use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use jorb_vmm::{Tap, VmShape, VmState};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

use crate::job::OrbId;
use crate::settings::Settings;
use crate::{limits, run};

/// Slot S is `10.77.S.0/30`: the host takes `.1`, the guest `.2`.
const SLOT_NETWORK: [u8; 2] = [10, 77];
const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);
const TAP_PREFIX: &str = "jorb";

const CMDLINE_FIXED: &str = "console=ttyAMA0 root=/dev/vda rw init=/jorb-init";
const GUEST_INTERFACE: &str = "eth0";
/// Public resolvers: an orb may not reach the host's.
const DNS: &str = "1.1.1.1:9.9.9.9";

/// The table holding the network policy, from the Nix module.
const POLICY_TABLE: [&str; 2] = ["inet", "jorb"];

#[derive(Debug, Clone, Copy)]
pub struct Slot(u8);

impl Slot {
    /// No free slot is the capacity error, and there is no other.
    pub fn lowest_free(settings: &Settings) -> Result<Self> {
        (0..=u8::MAX)
            .take(usize::from(settings.slots))
            .map(Self)
            .find(|slot| !Path::new("/sys/class/net").join(slot.tap()).exists())
            .with_context(|| format!("no free slot: all {} are taken", settings.slots))
    }

    pub fn index(self) -> u8 {
        self.0
    }

    fn tap(self) -> String {
        format!("{TAP_PREFIX}{}", self.0)
    }

    fn address(self, host_part: u8) -> Ipv4Addr {
        let [first, second] = SLOT_NETWORK;
        Ipv4Addr::new(first, second, self.0, host_part)
    }
}

fn cmdline(slot: Slot, orb: &OrbId) -> String {
    let (host, guest) = (slot.address(1), slot.address(2));
    format!("{CMDLINE_FIXED} ip={guest}::{host}:{NETMASK}:{orb}:{GUEST_INTERFACE}:off:{DNS}")
}

/// What the orb's unit runs: the VMM, waiting on its API socket for a VM.
pub fn vmm_command(settings: &Settings, orb: &OrbId) -> Vec<String> {
    vec![
        settings.cloud_hypervisor.display().to_string(),
        format!("--api-socket={}", settings.api_socket_path(orb).display()),
    ]
}

/// Waits for the VMM to listen, then creates and boots the orb's VM in
/// `slot`. Returns once the VMM reports it running.
pub async fn boot(settings: &Settings, orb: &OrbId, slot: Slot) -> Result<()> {
    let socket = settings.api_socket_path(orb);
    let deadline = Instant::now() + limits::API_SOCKET_WAIT;
    while !listening(&socket).await? {
        ensure!(
            Instant::now() < deadline,
            "nothing listened on {} in {:?}",
            socket.display(),
            limits::API_SOCKET_WAIT
        );
        sleep(limits::API_SOCKET_POLL).await;
    }

    let cmdline = cmdline(slot, orb);
    let tap = slot.tap();
    let shape = VmShape {
        kernel: &settings.kernel,
        cmdline: &cmdline,
        cpus: settings.vcpus,
        memory_mib: settings.memory_mib,
        root_disk: &settings.disk_path(orb),
        read_only_disk: &settings.config_disk_path(orb),
        tap: Tap {
            name: &tap,
            host_address: slot.address(1),
            netmask: NETMASK,
        },
    };
    Ok(jorb_vmm::boot(&socket, &shape).await?)
}

/// The socket file appears before the VMM listens on it, so a missing file,
/// a refused connection and a full backlog all mean "not yet".
async fn listening(socket: &Path) -> Result<bool> {
    match timeout(limits::API_SOCKET_POLL, UnixStream::connect(socket)).await {
        Err(_) => Ok(false),
        Ok(Ok(_)) => Ok(true),
        Ok(Err(error))
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Ok(Err(error)) => Err(error).with_context(|| format!("connecting to {}", socket.display())),
    }
}

/// Ok only when the orb's VMM says its VM is running; otherwise, why not.
pub async fn confirm_running(settings: &Settings, orb: &OrbId) -> Result<()> {
    let state = jorb_vmm::state(&settings.api_socket_path(orb)).await?;
    ensure!(state == VmState::Running, "the VM is {state:?}");
    Ok(())
}

/// The policy only drops, so without its table an orb would reach the host
/// and the LAN and nothing else would notice.
pub async fn ensure_network_policy() -> Result<()> {
    let [family, name] = POLICY_TABLE;
    run("nft", &["list", "table", family, name])
        .await
        .with_context(|| format!("the network policy table {family} {name} is not loaded"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cmdline_is_the_fixed_interface() {
        assert_eq!(
            cmdline(Slot(7), &"amp-1".parse().unwrap()),
            "console=ttyAMA0 root=/dev/vda rw init=/jorb-init \
             ip=10.77.7.2::10.77.7.1:255.255.255.252:amp-1:eth0:off:1.1.1.1:9.9.9.9"
        );
    }

    #[tokio::test]
    async fn a_socket_is_ready_only_once_something_listens() {
        let directory = std::env::temp_dir().join("jorb-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("vmm.sock");
        let _ = std::fs::remove_file(&socket);

        assert!(!listening(&socket).await.unwrap(), "no file");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(listening(&socket).await.unwrap(), "a listener");
        drop(listener);
        assert!(
            !listening(&socket).await.unwrap(),
            "a file nobody listens on"
        );
    }
}
