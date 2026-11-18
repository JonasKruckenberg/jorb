//! A Cloud Hypervisor client, narrowed to the one VM shape jorb boots.
//!
//! The crate refuses to know about systemd, slots, orbs, Amp or where any of
//! the paths it is handed came from. Nothing from the generated client is
//! re-exported, so the rest of jorb can only express the shape this crate
//! allows.
//!
//! An API socket is the only name for a VM: [`boot`] and [`state`] both take
//! one, and there is no handle type to keep in step with it. Nothing here
//! stops or deletes a VM, and nothing cleans up on failure. A VM's lifetime
//! belongs to its systemd unit, and jorb is crash-only; see the failure model
//! in `AGENTS.md`.

mod generated;

use std::fmt::Display;
use std::net::Ipv4Addr;
use std::num::{NonZeroU8, NonZeroU32, NonZeroU64};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use generated::types;

/// How long one request to Cloud Hypervisor's API may take, connection
/// included. The API is a unix socket on the same host and every call is a
/// few hundred bytes of JSON, so anything slower is a VMM that has stopped
/// answering.
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Cloud Hypervisor sizes memory in bytes; jorb thinks in MiB.
const BYTES_PER_MIB: i64 = 1024 * 1024;

/// The fixed VM shape. Every field is required, so a VM with no disk or no
/// network cannot be asked for. Huge pages are always off, serial goes to
/// stdout, the virtio console is off, and a size-0 balloon reports free pages.
#[derive(Debug, Clone, Copy)]
pub struct VmShape<'a> {
    pub kernel: &'a Path,
    pub cmdline: &'a str,
    pub cpus: NonZeroU8,
    pub memory_mib: NonZeroU32,
    /// Writable; the guest sees it as `/dev/vda`.
    pub root_disk: &'a Path,
    /// Read-only; the guest sees it as `/dev/vdb`.
    pub read_only_disk: &'a Path,
    pub tap: Tap<'a>,
}

/// The guest's one interface. Cloud Hypervisor creates the tap if the host
/// does not have it yet.
#[derive(Debug, Clone, Copy)]
pub struct Tap<'a> {
    pub name: &'a str,
    pub host_address: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

/// What Cloud Hypervisor says about the VM behind an API socket.
///
/// `Absent` is this crate's own, and it is the single answer for every way of
/// having no VM: no socket, a socket nothing is listening on, and a VMM that
/// is up but was never given a VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    Absent,
    Created,
    Running,
    Shutdown,
    Paused,
    BreakPoint,
}

/// Creates the VM on the VMM listening at `socket`, boots it, and returns only
/// once the VMM reports it running.
pub async fn boot(socket: &Path, shape: &VmShape<'_>) -> Result<()> {
    let client = client(socket)?;
    let at = || socket.display().to_string();
    client
        .create_vm()
        .body(configuration(shape)?)
        .send()
        .await
        .with_context(|| format!("vm.create on {}", at()))?;
    client
        .boot_vm()
        .send()
        .await
        .with_context(|| format!("vm.boot on {}", at()))?;

    let state = state(socket).await?;
    ensure!(
        state == VmState::Running,
        "the VM on {} is {state:?} after booting",
        at()
    );
    Ok(())
}

/// What the VMM at `socket` says about its VM, in one request.
pub async fn state(socket: &Path) -> Result<VmState> {
    if !socket.try_exists()? {
        return Ok(VmState::Absent);
    }
    match client(socket)?.vm_info().send().await {
        Ok(info) => Ok(VmState::from(info.into_inner().state)),
        // A socket with no listener is debris a dead VMM left behind.
        Err(progenitor_client::Error::CommunicationError(source)) if source.is_connect() => {
            Ok(VmState::Absent)
        }
        // The VMM answers 404 until a VM is created on it.
        Err(progenitor_client::Error::ErrorResponse(response))
            if response.status() == reqwest::StatusCode::NOT_FOUND =>
        {
            Ok(VmState::Absent)
        }
        Err(error) => Err(error).with_context(|| format!("vm.info on {}", socket.display())),
    }
}

fn client(socket: &Path) -> Result<generated::Client> {
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .timeout(API_REQUEST_TIMEOUT)
        .connect_timeout(API_REQUEST_TIMEOUT)
        .build()?;
    // The host in the URL is never resolved: every connection goes to the socket.
    Ok(generated::Client::new_with_client(
        "http://localhost/api/v1",
        client,
    ))
}

/// The shape as Cloud Hypervisor's API wants it.
fn configuration(shape: &VmShape<'_>) -> Result<types::VmConfig> {
    let cpus = NonZeroU64::from(shape.cpus);

    let mut disks = Vec::new();
    for (path, read_only) in [(shape.root_disk, false), (shape.read_only_disk, true)] {
        disks.push(assembled(
            types::DiskConfig::builder()
                .path(Some(path_string(path)))
                .readonly(read_only)
                // Cloud Hypervisor refuses to boot a disk whose image type it
                // was not told ("Image type required for disk", a 400 on
                // vm.boot). Both disks are raw files.
                .image_type(Some(types::ImageType::Raw)),
        )?);
    }

    assembled(
        types::VmConfig::builder()
            .payload(types::PayloadConfig {
                kernel: Some(path_string(shape.kernel)),
                cmdline: Some(shape.cmdline.to_owned()),
                ..Default::default()
            })
            .cpus(Some(assembled(
                types::CpusConfig::builder()
                    .boot_vcpus(cpus)
                    .max_vcpus(cpus),
            )?))
            .memory(Some(assembled(
                types::MemoryConfig::builder()
                    .size(i64::from(shape.memory_mib.get()) * BYTES_PER_MIB)
                    .hugepages(false)
                    .thp(false),
            )?))
            .disks(disks)
            .net(vec![assembled(
                types::NetConfig::builder()
                    .tap(Some(shape.tap.name.to_owned()))
                    .ip(shape.tap.host_address.to_string())
                    .mask(shape.tap.netmask.to_string()),
            )?])
            .balloon(Some(assembled(
                types::BalloonConfig::builder()
                    .size(0)
                    .free_page_reporting(true),
            )?))
            .serial(Some(assembled(
                types::SerialConfig::builder().mode(types::ConsoleMode::Tty),
            )?))
            .console(Some(assembled(
                types::ConsoleConfig::builder().mode(types::ConsoleMode::Off),
            )?)),
    )
}

impl From<types::VmState> for VmState {
    fn from(state: types::VmState) -> Self {
        match state {
            types::VmState::Created => Self::Created,
            types::VmState::Running => Self::Running,
            types::VmState::Shutdown => Self::Shutdown,
            types::VmState::Paused => Self::Paused,
            types::VmState::BreakPoint => Self::BreakPoint,
        }
    }
}

/// Turns a generated builder into its config, naming the field it refused.
fn assembled<B, T>(builder: B) -> Result<T>
where
    B: TryInto<T>,
    B::Error: Display,
{
    builder
        .try_into()
        .map_err(|message| anyhow!("assembling the VM configuration: {message}"))
}

/// Paths reach Cloud Hypervisor as strings. A path that is not UTF-8 would be
/// a path jorb did not make, so this is lossy rather than fallible.
fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape from the Fixed interfaces section of AGENTS.md, with the v1
    /// sizing.
    fn fixed_shape() -> VmShape<'static> {
        VmShape {
            kernel: Path::new("/var/lib/jorb/kernel"),
            cmdline: "console=ttyAMA0 root=/dev/vda rw init=/jorb-init \
                      ip=10.77.0.2::10.77.0.1:255.255.255.252:orb:eth0:off:1.1.1.1:9.9.9.9",
            cpus: NonZeroU8::new(4).unwrap(),
            memory_mib: NonZeroU32::new(6144).unwrap(),
            root_disk: Path::new("/var/lib/jorb/disks/orb.raw"),
            read_only_disk: Path::new("/run/jorb/orb.cfg"),
            tap: Tap {
                name: "jorb0",
                host_address: Ipv4Addr::new(10, 77, 0, 1),
                netmask: Ipv4Addr::new(255, 255, 255, 252),
            },
        }
    }

    /// The golden file carries every field Cloud Hypervisor defaults, disk
    /// order included.
    #[test]
    fn the_fixed_shape_is_the_checked_in_json() {
        let sent = serde_json::to_value(configuration(&fixed_shape()).unwrap()).unwrap();
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../tests/vm-shape.json")).unwrap();

        assert_eq!(
            serde_json::to_string_pretty(&sent).unwrap(),
            serde_json::to_string_pretty(&golden).unwrap(),
        );
    }

    #[test]
    fn a_socket_that_is_not_there_has_no_vm() {
        let state = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(state(Path::new("/run/jorb/definitely-not-here.sock")))
            .unwrap();

        assert_eq!(state, VmState::Absent);
    }
}
