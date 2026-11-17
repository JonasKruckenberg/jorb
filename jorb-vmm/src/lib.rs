//! A Cloud Hypervisor client, narrowed to the one VM shape jorb boots.
//!
//! The crate refuses to know about systemd, slots, orbs, Amp or where any of
//! the paths it is handed came from. It takes a kernel, a cmdline, disks and a
//! tap, and gives back a [`Vm`] that is running. Nothing from the generated
//! client is re-exported, so Cloud Hypervisor's API surface stays an
//! implementation detail and the rest of jorb can only express the shape this
//! crate allows.
//!
//! There is no method to stop or delete a VM: jorb is crash-only, and a VM
//! stops because its unit stops. See the failure model in `AGENTS.md`.

mod generated;
mod limits;

use std::fmt::Display;
use std::net::Ipv4Addr;
use std::num::{NonZeroU8, NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};

use generated::types;

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

/// A Cloud Hypervisor that has a VM. A `Vm` exists only for a VM that was
/// created, so there is no "maybe booted" state to handle.
#[derive(Debug)]
pub struct Vm {
    socket: PathBuf,
    client: generated::Client,
}

/// What Cloud Hypervisor says about the VM behind an API socket.
///
/// `Absent` is this crate's own: the VMM answered but has no VM, which is what
/// a job that died between starting the unit and creating the VM leaves
/// behind.
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

    /// A piece of the VM configuration was not accepted by the generated
    /// types, which means this crate asked for something it cannot express.
    #[error("the VM configuration could not be assembled: {message}")]
    Configuration { message: String },
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
    #[must_use]
    pub fn free_page_reporting(mut self) -> Self {
        self.free_page_reporting = true;
        self
    }

    /// Creates the VM on the Cloud Hypervisor listening at `socket`, then
    /// boots it.
    ///
    /// Nothing is cleaned up on failure, on purpose: the next job's recover
    /// removes whatever is left. See the failure model in `AGENTS.md`.
    pub async fn boot(self, socket: &Path) -> Result<Vm, Error> {
        let vm = Vm::new(socket)?;
        let configuration = self.configuration()?;

        vm.client
            .create_vm()
            .body(configuration)
            .send()
            .await
            .map_err(|source| vm.failed("vm.create", source))?;

        vm.client
            .boot_vm()
            .send()
            .await
            .map_err(|source| vm.failed("vm.boot", source))?;

        Ok(vm)
    }

    /// The VM shape as Cloud Hypervisor's API wants it.
    fn configuration(&self) -> Result<types::VmConfig, Error> {
        let cpus = NonZeroU64::from(self.cpus);

        let mut disks = Vec::with_capacity(self.disks.len());
        for disk in &self.disks {
            disks.push(assembled(
                types::DiskConfig::builder()
                    .path(Some(path_string(&disk.path)))
                    .readonly(disk.read_only),
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
                        .size(i64::from(self.memory_mib.get()) * 1024 * 1024)
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

impl Vm {
    /// The VM behind `socket`, or `None` when there is nothing listening:
    /// either the socket is gone, or it is a file a dead job left behind.
    pub async fn attach(socket: &Path) -> Result<Option<Self>, Error> {
        let exists = socket.try_exists().map_err(|source| Error::Socket {
            socket: socket.to_path_buf(),
            source,
        })?;
        if !exists {
            return Ok(None);
        }

        let vm = Self::new(socket)?;

        match vm.client.vmm_ping().send().await {
            Ok(_) => Ok(Some(vm)),
            // A socket with no listener is debris, not a failure.
            Err(progenitor_client::Error::CommunicationError(source)) if source.is_connect() => {
                Ok(None)
            }
            Err(source) => Err(vm.failed("vmm.ping", source)),
        }
    }

    /// What the VMM says about its VM right now.
    pub async fn state(&self) -> Result<VmState, Error> {
        match self.client.vm_info().send().await {
            Ok(info) => Ok(VmState::from(info.into_inner().state)),
            // The VMM answers 404 until a VM is created on it.
            Err(progenitor_client::Error::ErrorResponse(response))
                if response.status() == reqwest::StatusCode::NOT_FOUND =>
            {
                Ok(VmState::Absent)
            }
            Err(source) => Err(self.failed("vm.info", source)),
        }
    }

    /// Binds a client to the socket. No request is made, so this says nothing
    /// about whether anything is listening.
    fn new(socket: &Path) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .unix_socket(socket)
            .timeout(limits::API_REQUEST_TIMEOUT)
            .connect_timeout(limits::API_REQUEST_TIMEOUT)
            .build()
            .map_err(|source| Error::Transport {
                socket: socket.to_path_buf(),
                source,
            })?;

        Ok(Self {
            socket: socket.to_path_buf(),
            // The host part of the URL is never resolved: every connection
            // goes to the socket the client was built with.
            client: generated::Client::new_with_client("http://localhost/api/v1", http),
        })
    }

    fn failed(&self, operation: &'static str, source: progenitor_client::Error) -> Error {
        Error::Request {
            socket: self.socket.clone(),
            operation,
            source: Box::new(source),
        }
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
    fn disks_keep_the_order_they_were_added_in() {
        let configuration = fixed_shape().configuration().unwrap();
        let paths: Vec<_> = configuration
            .disks
            .iter()
            .map(|disk| (disk.path.clone().unwrap(), disk.readonly))
            .collect();

        assert_eq!(
            paths,
            vec![
                ("/var/lib/jorb/disks/orb.raw".to_owned(), false),
                ("/run/jorb/orb.cfg".to_owned(), true),
            ]
        );
    }

    #[test]
    fn a_vm_without_a_socket_is_not_attached() {
        let absent = Path::new("/run/jorb/definitely-not-here.sock");
        let attached = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(Vm::attach(absent))
            .unwrap();

        assert!(attached.is_none());
    }
}
