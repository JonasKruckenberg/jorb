//! Prints the version of the Cloud Hypervisor listening on an API socket.
//!
//! Usage: `cargo run -p jorb-vmm --example ping -- /run/jorb/<id>.sock`
#![allow(
    clippy::print_stdout,
    reason = "the printed version is what this example is for"
)]

use std::env;
use std::error::Error;
use std::path::PathBuf;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let socket = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: ping <api socket>")?;

    println!("{}", jorb_vmm::vmm_version(&socket).await?);

    Ok(())
}
