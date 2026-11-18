//! A Cloud Hypervisor client, narrowed to the one VM shape jorb boots.
//!
//! The crate refuses to know about systemd, slots, orbs, Amp or where any of
//! the paths it is handed came from. It takes a kernel, a cmdline, disks and a
//! tap, and boots that shape. Nothing from the generated client is
//! re-exported, so Cloud Hypervisor's API surface stays an implementation
//! detail and the rest of jorb can only express the shape this crate allows.
//!
//! An API socket is the only name for a VM here. There is no handle type: a
//! handle would be a second way to say "this VM", and the one question anyone
//! asks is [`state`]. There is no method to stop or delete either, because
//! jorb is crash-only and a VM stops because its unit stops. See the failure
//! model in `AGENTS.md`.

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

    /// A piece of the VM configuration was not accepted by the generated
    /// types, which means this crate asked for something it cannot express.
    #[error("the VM configuration could not be assembled: {message}")]
    Configuration { message: String },
}

/// What the VMM behind `socket` says about its VM, in one request.
///
/// Every way of having no VM answers [`VmState::Absent`], so a caller that
/// wants to know whether an orb is up has one question to ask and one value to
/// compare against.
pub async fn state(socket: &Path) -> Result<VmState, Error> {
    let exists = socket.try_exists().map_err(|source| Error::Socket {
        socket: socket.to_path_buf(),
        source,
    })?;
    if !exists {
        return Ok(VmState::Absent);
    }

    match client(socket)?.vm_info().send().await {
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
        Err(source) => Err(failed(socket, "vm.info", source)),
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
    // ASSUMES A2: disks reach the guest in the order they are attached, so
    // the root disk is /dev/vda and the config disk /dev/vdb. G4 checks it.
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
    // ASSUMES A5: free page reporting actually returns the memory to the
    // host rather than only telling the guest it may. G5 checks it.
    #[must_use]
    pub fn free_page_reporting(mut self) -> Self {
        self.free_page_reporting = true;
        self
    }

    /// Creates the VM on the Cloud Hypervisor listening at `socket`, then
    /// boots it.
    ///
    /// Returning nothing is the point: asking the VMM is the only honest way
    /// to learn that a VM is running, and [`state`] is where that question
    /// goes. Nothing is cleaned up on failure, on purpose; the next job's
    /// recover removes whatever is left.
    pub async fn boot(self, socket: &Path) -> Result<(), Error> {
        let client = client(socket)?;
        let configuration = self.configuration()?;

        client
            .create_vm()
            .body(configuration)
            .send()
            .await
            .map_err(|source| failed(socket, "vm.create", source))?;

        client
            .boot_vm()
            .send()
            .await
            .map_err(|source| failed(socket, "vm.boot", source))?;

        Ok(())
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
                        .size(i64::from(self.memory_mib.get()) * limits::BYTES_PER_MIB)
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

/// Binds a client to the socket. No request is made, so this says nothing
/// about whether anything is listening.
fn client(socket: &Path) -> Result<generated::Client, Error> {
    let http = reqwest::Client::builder()
        .unix_socket(socket)
        .timeout(limits::API_REQUEST_TIMEOUT)
        .connect_timeout(limits::API_REQUEST_TIMEOUT)
        .build()
        .map_err(|source| Error::Transport {
            socket: socket.to_path_buf(),
            source,
        })?;

    // The host part of the URL is never resolved: every connection goes to
    // the socket the client was built with.
    Ok(generated::Client::new_with_client(
        "http://localhost/api/v1",
        http,
    ))
}

fn failed(socket: &Path, operation: &'static str, source: progenitor_client::Error) -> Error {
    Error::Request {
        socket: socket.to_path_buf(),
        operation,
        source: Box::new(source),
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
    /// order included, which is what A2 rests on.
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
        let state = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(state(Path::new("/run/jorb/definitely-not-here.sock")))
            .unwrap();

        assert_eq!(state, VmState::Absent);
    }
}
