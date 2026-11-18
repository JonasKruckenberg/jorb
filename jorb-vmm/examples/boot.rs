//! Boots the fixed VM shape on a Cloud Hypervisor that is already listening.
//!
//! Usage: `cargo run -p jorb-vmm --example boot -- <socket> <kernel> <root disk> <config disk> <tap> <slot>`
#![allow(
    clippy::print_stdout,
    reason = "what this example prints is what it is for"
)]

use std::env;
use std::error::Error;
use std::net::Ipv4Addr;
use std::num::{NonZeroU8, NonZeroU32};
use std::path::PathBuf;

use jorb_vmm::{Tap, VmShape};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<PathBuf> = env::args_os().skip(1).map(PathBuf::from).collect();
    let [socket, kernel, root_disk, config_disk, tap, slot] = arguments.as_slice() else {
        return Err("usage: boot <socket> <kernel> <root disk> <config disk> <tap> <slot>".into());
    };

    let tap = tap.to_string_lossy();
    let slot: u8 = slot.to_string_lossy().parse()?;
    let cmdline = format!(
        "console=ttyAMA0 root=/dev/vda rw init=/init \
         ip=10.77.{slot}.2::10.77.{slot}.1:255.255.255.252:boot-example:eth0:off:1.1.1.1:9.9.9.9"
    );

    let shape = VmShape {
        kernel,
        cmdline: &cmdline,
        cpus: NonZeroU8::new(4).ok_or("four is not zero")?,
        memory_mib: NonZeroU32::new(6144).ok_or("6144 is not zero")?,
        root_disk,
        read_only_disk: config_disk,
        tap: Tap {
            name: &tap,
            host_address: Ipv4Addr::new(10, 77, slot, 1),
            netmask: Ipv4Addr::new(255, 255, 255, 252),
        },
    };

    jorb_vmm::boot(socket, &shape).await?;
    println!("{:?}", jorb_vmm::state(socket).await?);

    Ok(())
}
