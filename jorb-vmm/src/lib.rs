//! A Cloud Hypervisor client, narrowed to the one VM shape jorb boots.
//!
//! The crate refuses to know about systemd, slots, orbs or Amp: it takes a
//! kernel, a cmdline, disks and a tap, and it returns a `Vm` that is running.
//! The generated client stays private so Cloud Hypervisor's API surface cannot
//! leak into callers.

mod generated;
mod limits;

use std::path::{Path, PathBuf};

/// What can go wrong talking to a Cloud Hypervisor over its API socket.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The socket is there but no HTTP client could be bound to it.
    #[error("no HTTP client could be bound to the API socket at {socket}")]
    Transport {
        socket: PathBuf,
        #[source]
        source: reqwest::Error,
    },

    /// The request did not arrive, or the answer was not what the API promised.
    //
    // Boxed because progenitor's error is 160 bytes, which would make every
    // Result in this crate that wide.
    #[error("the request to the VMM at {socket} failed")]
    Request {
        socket: PathBuf,
        #[source]
        source: Box<progenitor_client::Error>,
    },
}

/// The version string of the Cloud Hypervisor listening on `socket`.
///
/// T2's spike: the one call that proves the generated client works over
/// reqwest's unix-socket transport. T4 replaces it with `Vm::attach`.
#[doc(hidden)]
pub async fn vmm_version(socket: &Path) -> Result<String, Error> {
    let http = reqwest::Client::builder()
        .unix_socket(socket)
        .build()
        .map_err(|source| Error::Transport {
            socket: socket.to_path_buf(),
            source,
        })?;

    // The host part of the URL is never resolved: every connection goes to the
    // socket the client was built with.
    let client = generated::Client::new_with_client("http://localhost/api/v1", http);

    let ping = client
        .vmm_ping()
        .send()
        .await
        .map_err(|source| Error::Request {
            socket: socket.to_path_buf(),
            source: Box::new(source),
        })?;

    Ok(ping.into_inner().version)
}
