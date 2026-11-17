//! Says what the Cloud Hypervisor on an API socket has, if anything.
//!
//! Usage: `cargo run -p jorb-vmm --example ping -- /run/jorb/<id>.sock`
#![allow(
    clippy::print_stdout,
    reason = "what this example prints is what it is for"
)]

use std::env;
use std::error::Error;
use std::path::PathBuf;

use jorb_vmm::Vm;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let socket = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: ping <api socket>")?;

    match Vm::attach(&socket).await? {
        Some(vm) => println!("{:?}", vm.state().await?),
        None => println!("nothing is listening on {}", socket.display()),
    }

    Ok(())
}
