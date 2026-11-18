//! A Cloud Hypervisor client, narrowed to the one VM shape jorb boots.
//!
//! The crate refuses to know about systemd, slots, orbs, Amp or where any of
//! the paths it is handed came from. Nothing from the generated client is
//! re-exported, so Cloud Hypervisor's API surface stays an implementation
//! detail and the rest of jorb can only express the shape this crate allows.
//!
//! Three types, in the order a caller meets them. [`Vmm`] is the hypervisor
//! listening on an API socket, which may or may not have been given a VM.
//! [`VmBuilder`] assembles the shape jorb boots. [`Vm`] is what booting
//! returns, and it exists only for a VM the same call confirmed running.
//!
//! [`Vm`] has no `Drop`. A VM outlives the jorb process that booted it: its
//! lifetime belongs to the systemd unit, and jorb is crash-only, so tidying up
//! on the way out would be a second path that a kill skips. That is also why
//! there is no way to stop or delete a VM here. See the failure model in
//! `AGENTS.md`.

mod generated;

use std::fmt::Display;
use std::net::Ipv4Addr;
use std::num::{NonZeroU8, NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::time::Duration;

use generated::types;

/// How long one request to Cloud Hypervisor's API may take, connection
/// included.
///
/// The API is a unix socket on the same host and every call jorb makes is a
/// few hundred bytes of JSON, so anything slower than this is a VMM that has
/// stopped answering rather than one that is busy. Short enough that a job can
/// make several calls and still finish well inside Amp's five minutes.
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Cloud Hypervisor sizes memory in bytes; jorb thinks in MiB.
const BYTES_PER_MIB: i64 = 1024 * 1024;

/// A Cloud Hypervisor listening on an API socket.
///
/// Holding one says nothing about whether anything is listening, or whether a
/// VM was ever created: that is what [`Vmm::state`] is for.
#[derive(Debug)]
pub struct Vmm {
    socket: PathBuf,
    client: generated::Client,
}

/// The VM shape jorb boots, assembled one piece at a time.
///
/// `cpus` and `memory_mib` are arguments to [`VmBuilder::new`] rather than
/// methods, because a VM without them is not a shape anyone meant to ask for.
/// Their types differ so the two numbers cannot be swapped by accident.
#[derive(Debug, Clone)]
pub struct VmBuilder {
    kernel: PathBuf,
    cmdline: String,
    cpus: NonZeroU8,
    memory_mib: NonZeroU32,
    huge_pages: bool,
    disks: Vec<Disk>,
    tap: Option<Tap>,
    free_page_reporting: bool,
}

/// A VM that was created, booted, and reported running by the VMM it runs on.
///
/// Borrowing the [`Vmm`] is what keeps the two from drifting apart: the VM
/// cannot outlive the client that knows how to reach it.
#[derive(Debug)]
pub struct Vm<'a> {
    vmm: &'a Vmm,
}

#[derive(Debug, Clone)]
struct Disk {
    path: PathBuf,
    read_only: bool,
}

#[derive(Debug, Clone)]
struct Tap {
    name: String,
    host_address: Ipv4Addr,
    netmask: Ipv4Addr,
}

/// What Cloud Hypervisor says about the VM behind an API socket.
///
/// `Absent` is this crate's own, and it is the single answer for every way of
/// having no VM: no socket, a socket nothing is listening on, and a VMM that
/// is up but was never given a VM. A job that died between starting the unit
/// and creating the VM leaves one of the last two behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    Absent,
    Created,
    Running,
    Shutdown,
    Paused,
    BreakPoint,
}

/// What can go wrong talking to a Cloud Hypervisor over its API socket.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The socket could not be looked at.
    #[error("the API socket at {socket} could not be read")]
    Socket {
        socket: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The socket is there but no HTTP client could be bound to it.
    #[error("no HTTP client could be bound to the API socket at {socket}")]
    Transport {
        socket: PathBuf,
        #[source]
        source: reqwest::Error,
    },

    /// A request did not arrive, or the answer was not what the API promised.
    //
    // Boxed because progenitor's error is 160 bytes, which would otherwise
    // make every Result in this crate that wide.
    #[error("the {operation} request to the VMM at {socket} failed")]
    Request {
        socket: PathBuf,
        operation: &'static str,
        #[source]
        source: Box<progenitor_client::Error>,
    },

    /// Create and boot both succeeded and the VMM still does not report a
    /// running VM, so there is nothing to hand back.
    #[error("the VM on {socket} is {state:?} rather than running after being booted")]
    NotRunning { socket: PathBuf, state: VmState },

    /// A piece of the VM configuration was not accepted by the generated
    /// types, which means this crate asked for something it cannot express.
    #[error("the VM configuration could not be assembled: {message}")]
    Configuration { message: String },
}

impl Vmm {
    /// Binds a client to `socket`. No request is made here.
    pub fn at(socket: &Path) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .unix_socket(socket)
            .timeout(API_REQUEST_TIMEOUT)
            .connect_timeout(API_REQUEST_TIMEOUT)
            .build()
            .map_err(|source| Error::Transport {
                socket: socket.to_path_buf(),
                source,
            })?;

        Ok(Self {
            socket: socket.to_path_buf(),
            // The host part of the URL is never resolved: every connection
            // goes to the socket the client was built with.
            client: generated::Client::new_with_client("http://localhost/api/v1", client),
        })
    }

    /// What this VMM says about its VM, in one request.
    ///
    /// Every way of having no VM answers [`VmState::Absent`], so a caller has
    /// one question to ask and one value to compare against.
    pub async fn state(&self) -> Result<VmState, Error> {
        let exists = self.socket.try_exists().map_err(|source| Error::Socket {
            socket: self.socket.clone(),
            source,
        })?;
        if !exists {
            return Ok(VmState::Absent);
        }

        match self.client.vm_info().send().await {
            Ok(info) => Ok(VmState::from(info.into_inner().state)),
            // A socket with no listener is debris a dead job left behind.
            Err(progenitor_client::Error::CommunicationError(source)) if source.is_connect() => {
                Ok(VmState::Absent)
            }
            // The VMM answers 404 until a VM is created on it.
            Err(progenitor_client::Error::ErrorResponse(response))
                if response.status() == reqwest::StatusCode::NOT_FOUND =>
            {
                Ok(VmState::Absent)
            }
            Err(source) => Err(self.failed("vm.info", source)),
        }
    }

    fn failed(&self, operation: &'static str, source: progenitor_client::Error) -> Error {
        Error::Request {
            socket: self.socket.clone(),
            operation,
            source: Box::new(source),
        }
    }
}

impl VmBuilder {
    /// Serial goes to stdout and the virtio console is off, always: the serial
    /// line is how a guest's output reaches the host's journal.
    pub fn new(kernel: &Path, cmdline: &str, cpus: NonZeroU8, memory_mib: NonZeroU32) -> Self {
        Self {
            kernel: kernel.to_path_buf(),
            cmdline: cmdline.to_owned(),
            cpus,
            memory_mib,
            huge_pages: true,
            disks: Vec::new(),
            tap: None,
            free_page_reporting: false,
        }
    }

    /// Keeps the guest's memory in ordinary pages.
    ///
    /// A huge page is the host's PMD size, which on a 16K-page host is 32 MiB
    /// (G2), so a guest on huge pages can only ever give memory back 32 MiB at
    /// a time.
    #[must_use]
    pub fn without_huge_pages(mut self) -> Self {
        self.huge_pages = false;
        self
    }

    /// Adds a writable disk. Disks reach the guest as `/dev/vda`, `/dev/vdb`
    /// and so on, in the order they were added here.
    #[must_use]
    pub fn disk(mut self, path: &Path) -> Self {
        self.disks.push(Disk {
            path: path.to_path_buf(),
            read_only: false,
        });
        self
    }

    /// Adds a read-only disk, in the same order as [`VmBuilder::disk`].
    //
    // A2, verified 2026-11-18: disks reach the guest in attach order, so the
    // root disk is /dev/vda and the config disk /dev/vdb.
    #[must_use]
    pub fn read_only_disk(mut self, path: &Path) -> Self {
        self.disks.push(Disk {
            path: path.to_path_buf(),
            read_only: true,
        });
        self
    }

    /// Gives the guest one network interface on `name`, which Cloud Hypervisor
    /// creates if the host does not have it yet.
    #[must_use]
    pub fn tap(mut self, name: &str, host_address: Ipv4Addr, netmask: Ipv4Addr) -> Self {
        self.tap = Some(Tap {
            name: name.to_owned(),
            host_address,
            netmask,
        });
        self
    }

    /// Asks the guest's balloon to report pages it frees, so freed guest
    /// memory stops being charged to the host.
    //
    // A5, verified by G5 on 2026-11-18: reporting returns the memory to the
    // host rather than only telling the guest it may.
    #[must_use]
    pub fn free_page_reporting(mut self) -> Self {
        self.free_page_reporting = true;
        self
    }

    /// Creates the VM on `vmm`, boots it, and asks the VMM whether it is
    /// running before handing back a [`Vm`].
    ///
    /// The last step is the point: create and boot can both be accepted and
    /// still leave a VM that is not running, and a caller holding a [`Vm`]
    /// should not have to ask again. jorb discards the [`Vm`] it gets, because
    /// its own job ends while the VM keeps running.
    ///
    /// Nothing is cleaned up on failure, on purpose; the next job's recover
    /// removes whatever is left.
    pub async fn boot<'v>(self, vmm: &'v Vmm) -> Result<Vm<'v>, Error> {
        let configuration = self.configuration()?;

        vmm.client
            .create_vm()
            .body(configuration)
            .send()
            .await
            .map_err(|source| vmm.failed("vm.create", source))?;

        vmm.client
            .boot_vm()
            .send()
            .await
            .map_err(|source| vmm.failed("vm.boot", source))?;

        let state = vmm.state().await?;
        if state != VmState::Running {
            return Err(Error::NotRunning {
                socket: vmm.socket.clone(),
                state,
            });
        }

        Ok(Vm { vmm })
    }

    /// The VM shape as Cloud Hypervisor's API wants it.
    fn configuration(&self) -> Result<types::VmConfig, Error> {
        let cpus = NonZeroU64::from(self.cpus);

        let mut disks = Vec::with_capacity(self.disks.len());
        for disk in &self.disks {
            disks.push(assembled(
                types::DiskConfig::builder()
                    .path(Some(path_string(&disk.path)))
                    .readonly(disk.read_only)
                    // Cloud Hypervisor 54.1 refuses to boot a disk whose
                    // image type it was not told ("Image type required for
                    // disk", a 400 on vm.boot). Every disk jorb attaches is a
                    // raw file: the base image, its reflink clones, and the
                    // config disk.
                    .image_type(Some(types::ImageType::Raw)),
            )?);
        }

        let net = match &self.tap {
            Some(tap) => vec![assembled(
                types::NetConfig::builder()
                    .tap(Some(tap.name.clone()))
                    .ip(tap.host_address.to_string())
                    .mask(tap.netmask.to_string()),
            )?],
            None => Vec::new(),
        };

        assembled(
            types::VmConfig::builder()
                .payload(types::PayloadConfig {
                    kernel: Some(path_string(&self.kernel)),
                    cmdline: Some(self.cmdline.clone()),
                    ..Default::default()
                })
                .cpus(Some(assembled(
                    types::CpusConfig::builder()
                        .boot_vcpus(cpus)
                        .max_vcpus(cpus),
                )?))
                .memory(Some(assembled(
                    types::MemoryConfig::builder()
                        .size(i64::from(self.memory_mib.get()) * BYTES_PER_MIB)
                        .hugepages(self.huge_pages)
                        .thp(self.huge_pages),
                )?))
                .disks(disks)
                .net(net)
                .balloon(Some(assembled(
                    types::BalloonConfig::builder()
                        .size(0)
                        .free_page_reporting(self.free_page_reporting),
                )?))
                .serial(Some(assembled(
                    types::SerialConfig::builder().mode(types::ConsoleMode::Tty),
                )?))
                .console(Some(assembled(
                    types::ConsoleConfig::builder().mode(types::ConsoleMode::Off),
                )?)),
        )
    }
}

impl Vm<'_> {
    /// What the VMM says about this VM now, which may no longer be running: a
    /// guest can switch itself off, and the host can stop its unit.
    pub async fn state(&self) -> Result<VmState, Error> {
        self.vmm.state().await
    }
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
fn assembled<B, T>(builder: B) -> Result<T, Error>
where
    B: TryInto<T>,
    B::Error: Display,
{
    builder.try_into().map_err(|message| Error::Configuration {
        message: message.to_string(),
    })
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
    /// sizing. The same shape the gates boot in tests/host.
    fn fixed_shape() -> VmBuilder {
        VmBuilder::new(
            Path::new("/var/lib/jorb/kernel"),
            "console=ttyAMA0 root=/dev/vda rw init=/jorb-init \
             ip=10.77.0.2::10.77.0.1:255.255.255.252:orb:eth0:off:1.1.1.1:9.9.9.9",
            NonZeroU8::new(4).unwrap(),
            NonZeroU32::new(6144).unwrap(),
        )
        .without_huge_pages()
        .disk(Path::new("/var/lib/jorb/disks/orb.raw"))
        .read_only_disk(Path::new("/run/jorb/orb.cfg"))
        .tap(
            "jorb0",
            Ipv4Addr::new(10, 77, 0, 1),
            Ipv4Addr::new(255, 255, 255, 252),
        )
        .free_page_reporting()
    }

    /// The golden file carries every field Cloud Hypervisor defaults, disk
    /// order included. A2 rests on that order.
    #[test]
    fn the_fixed_shape_is_the_checked_in_json() {
        let sent = serde_json::to_value(fixed_shape().configuration().unwrap()).unwrap();
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../tests/vm-shape.json")).unwrap();

        assert_eq!(
            serde_json::to_string_pretty(&sent).unwrap(),
            serde_json::to_string_pretty(&golden).unwrap(),
        );
    }

    #[test]
    fn a_socket_that_is_not_there_has_no_vm() {
        let vmm = Vmm::at(Path::new("/run/jorb/definitely-not-here.sock")).unwrap();
        let state = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(vmm.state())
            .unwrap();

        assert_eq!(state, VmState::Absent);
    }
}
