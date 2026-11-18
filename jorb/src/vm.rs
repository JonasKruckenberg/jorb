//! An orb's VM, from the host's side: booting it, powering it off, and the
//! files it lives by. This is the host half of booting an orb; `jorb-init` is
//! the guest half.
//!
//! The VMM runs as a systemd unit and is given the VM through its API socket.
//! A slot is free when its tap is absent: the VMM creates the tap when it
//! creates the VM, and the tap goes with the process.

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::net::Ipv4Addr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use jorb_config_disk::ConfigDisk;
use jorb_vmm::{Tap, VmShape};
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};
use tracing::{info, instrument};

use crate::request::OrbId;
use crate::settings::Settings;
use crate::{command, limits, systemd};

/// Slot S is `10.77.S.0/30`: the host takes `.1`, the guest `.2`.
const SLOT_NETWORK: [u8; 2] = [10, 77];
const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);
const TAP_PREFIX: &str = "jorb";
/// Public resolvers: an orb may not reach the host's.
const DNS: &str = "1.1.1.1:9.9.9.9";
/// The table holding the network policy, from the Nix module.
const POLICY_TABLE: [&str; 2] = ["inet", "jorb"];

/// Boots the orb's VM and returns once its VMM reports it running. Nothing is
/// undone if this fails; what it leaves is the next request's to clear.
#[instrument(skip_all, fields(slot))]
pub async fn boot(settings: &Settings, orb: &OrbId, config_disk: &ConfigDisk) -> Result<()> {
    // ===== Checking the network policy is loaded, so that no orb ever runs
    // unconfined. It only drops, so without its table nothing would notice;
    // first, because nothing below should happen without it.
    let [family, name] = POLICY_TABLE;
    command::run("nft", &["list", "table", family, name])
        .await
        .with_context(|| format!("the network policy table {family} {name} is not loaded"))?;

    // ===== Choosing a slot, so that the orb has a tap and addresses of its
    // own. Before the config disk, so that a full host leaves no token behind.
    let slot = Slot::lowest_free(settings)?;
    tracing::Span::current().record("slot", slot.0);

    // ===== Writing the config disk, so that the guest finds its token on
    // /dev/vdb. Mode 0600 on a tmpfs: the token never reaches a persistent disk.
    let socket = settings.api_socket_path(orb);
    remove_if_present(&socket)?;
    let path = settings.config_disk_path(orb);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    config_disk.write_to(&mut file)?;
    file.sync_all()?;

    // ===== Starting the VMM in the orb's unit, so that systemd owns its
    // lifetime and its journal. It opens the API socket and waits for a VM.
    let vmm = [
        settings.cloud_hypervisor.display().to_string(),
        format!("--api-socket={}", socket.display()),
    ];
    let invocation = systemd::start(orb, &vmm).await?;
    let journal = || format!("see journalctl _SYSTEMD_INVOCATION_ID={invocation}");
    info!(invocation, "started");

    // ===== Waiting for the API socket to listen, so that the VM can be asked
    // for. The file appears before the VMM listens on it.
    let deadline = Instant::now() + limits::API_SOCKET_WAIT;
    while !listening(&socket).await.with_context(journal)? {
        ensure!(
            Instant::now() < deadline,
            "nothing listened on {} within {:?}; {}",
            socket.display(),
            limits::API_SOCKET_WAIT,
            journal()
        );
        sleep(limits::API_SOCKET_POLL).await;
    }

    // ===== Creating and booting the VM, so that the guest kernel starts
    // jorb-init. Its network comes from the kernel's `ip=`, so nothing in the
    // guest configures an interface.
    let (host, guest) = (slot.address(1), slot.address(2));
    let cmdline = format!(
        "console=ttyAMA0 root=/dev/vda rw init=/jorb-init \
         ip={guest}::{host}:{NETMASK}:{orb}:eth0:off:{DNS}"
    );
    let tap = slot.tap();
    let shape = VmShape {
        kernel: &settings.kernel,
        cmdline: &cmdline,
        cpus: settings.vcpus,
        memory_mib: settings.memory_mib,
        root_disk: &settings.disk_path(orb),
        read_only_disk: &path,
        tap: Tap {
            name: &tap,
            host_address: host,
            netmask: NETMASK,
        },
    };
    jorb_vmm::boot(&socket, &shape)
        .await
        .with_context(journal)?;
    info!("running");
    Ok(())
}

/// Powers the VM off by stopping its unit, and takes its socket and config
/// disk away; the config disk holds the token. Pause is this: a power cut.
#[instrument(skip_all)]
pub async fn power_off(settings: &Settings, orb: &OrbId) -> Result<()> {
    systemd::stop(orb).await?;
    remove_if_present(&settings.api_socket_path(orb))?;
    remove_if_present(&settings.config_disk_path(orb))
}

/// Removes every config disk and socket that belongs to none of `running`. A
/// guest that powers off takes its unit with it, not its files.
pub fn remove_stray_files(settings: &Settings, running: &[OrbId]) -> Result<()> {
    for entry in std::fs::read_dir(&settings.run_directory)? {
        let path = entry?.path();
        let owner = path
            .file_stem()
            .and_then(|stem| stem.to_str()?.parse::<OrbId>().ok());
        if !owner.is_some_and(|orb| running.contains(&orb)) {
            remove_if_present(&path)?;
        }
    }
    Ok(())
}

/// Already gone is the same outcome, so every caller is safe to repeat.
pub fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot(u8);

impl Slot {
    /// No free slot is the capacity error, and there is no other.
    fn lowest_free(settings: &Settings) -> Result<Self> {
        (0..=u8::MAX)
            .take(usize::from(settings.slots))
            .map(Self)
            .find(|slot| !Path::new("/sys/class/net").join(slot.tap()).exists())
            .with_context(|| format!("no free slot: all {} are taken", settings.slots))
    }

    fn tap(self) -> String {
        format!("{TAP_PREFIX}{}", self.0)
    }

    fn address(self, host_part: u8) -> Ipv4Addr {
        let [first, second] = SLOT_NETWORK;
        Ipv4Addr::new(first, second, self.0, host_part)
    }
}

/// A missing file, a refused connection and a full backlog all mean "not yet".
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

#[cfg(test)]
mod tests {
    use super::*;

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
