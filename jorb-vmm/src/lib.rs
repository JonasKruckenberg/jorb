//! A Cloud Hypervisor client narrowed to what jorb asks: boot the one VM shape
//! jorb has, and say whether it is running. It knows nothing of systemd,
//! slots or Amp, and nothing generated is re-exported. The API socket is the
//! only name for a VM; nothing here stops a VM or cleans up after a failure.

mod generated;

use std::fmt::Display;
use std::net::Ipv4Addr;
use std::num::{NonZeroU8, NonZeroU32, NonZeroU64};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use generated::types;

/// One request over a local socket; anything slower is a VMM not answering.
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const BYTES_PER_MIB: i64 = 1024 * 1024;

/// Every field is required, so a VM with no disk or no network cannot be
/// asked for. Huge pages off, serial to stdout, console off, and a size-0
/// balloon that reports free pages are not choices.
#[derive(Debug, Clone, Copy)]
pub struct VmShape<'a> {
    pub kernel: &'a Path,
    pub cmdline: &'a str,
    pub cpus: NonZeroU8,
    pub memory_mib: NonZeroU32,
    /// Writable; `/dev/vda` in the guest.
    pub root_disk: &'a Path,
    /// Read-only; `/dev/vdb` in the guest.
    pub read_only_disk: &'a Path,
    pub tap: Tap<'a>,
}

#[derive(Debug, Clone, Copy)]
pub struct Tap<'a> {
    pub name: &'a str,
    pub host_address: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

/// Creates and boots the VM on the VMM at `socket`, and returns once the VMM
/// reports it running.
pub async fn boot(socket: &Path, shape: &VmShape<'_>) -> Result<()> {
    let client = client(socket)?;
    client
        .create_vm()
        .body(configuration(shape)?)
        .send()
        .await
        .with_context(|| format!("vm.create on {}", socket.display()))?;
    client
        .boot_vm()
        .send()
        .await
        .with_context(|| format!("vm.boot on {}", socket.display()))?;
    running(socket).await
}

/// Ok when the VMM at `socket` reports its VM running; otherwise, why not.
pub async fn running(socket: &Path) -> Result<()> {
    ensure!(
        socket.try_exists()?,
        "there is no API socket at {}",
        socket.display()
    );
    let info = client(socket)?
        .vm_info()
        .send()
        .await
        .with_context(|| format!("vm.info on {}", socket.display()))?;
    let state = info.into_inner().state;
    ensure!(state == types::VmState::Running, "the VM is {state:?}");
    Ok(())
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
}
