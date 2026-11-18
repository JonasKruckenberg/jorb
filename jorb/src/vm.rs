//! The fixed VM shape as Cloud Hypervisor's command line, and the one
//! question jorb asks a VMM: is your VM running?
//!
//! The whole configuration is on the command line, so the VMM boots the VM
//! as it starts and exits when the guest powers off. A slot is free when its
//! tap is absent: the VMM creates the tap and it goes with the process.

use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use tokio::time::sleep;

use crate::job::OrbId;
use crate::settings::Settings;
use crate::{limits, run};

/// Slot S is `10.77.S.0/30`: the host takes `.1`, the guest `.2`.
const SLOT_NETWORK: [u8; 2] = [10, 77];
const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 252);
const TAP_PREFIX: &str = "jorb";

const CMDLINE_FIXED: &str = "console=ttyAMA0 root=/dev/vda rw init=/jorb-init";
const GUEST_INTERFACE: &str = "eth0";
/// Public resolvers: an orb may not reach the host's.
const DNS: &str = "1.1.1.1:9.9.9.9";

/// The table holding the network policy, from the Nix module.
const POLICY_TABLE: [&str; 2] = ["inet", "jorb"];

#[derive(Debug, Clone, Copy)]
pub struct Slot(u8);

impl Slot {
    /// No free slot is the capacity error, and there is no other.
    pub fn lowest_free(settings: &Settings) -> Result<Self> {
        (0..=u8::MAX)
            .take(usize::from(settings.slots))
            .map(Self)
            .find(|slot| !Path::new("/sys/class/net").join(slot.tap()).exists())
            .with_context(|| format!("no free slot: all {} are taken", settings.slots))
    }

    pub fn index(self) -> u8 {
        self.0
    }

    fn tap(self) -> String {
        format!("{TAP_PREFIX}{}", self.0)
    }

    fn address(self, host_part: u8) -> Ipv4Addr {
        let [first, second] = SLOT_NETWORK;
        Ipv4Addr::new(first, second, self.0, host_part)
    }
}

/// The orb's VMM, as its unit runs it.
pub fn command(settings: &Settings, orb: &OrbId, slot: Slot) -> Vec<String> {
    let (host, guest) = (slot.address(1), slot.address(2));
    let path = |path: &Path| path.display().to_string();

    vec![
        path(&settings.cloud_hypervisor),
        format!("--api-socket={}", settings.api_socket_path(orb).display()),
        "--kernel".into(),
        path(&settings.kernel),
        "--cmdline".into(),
        format!("{CMDLINE_FIXED} ip={guest}::{host}:{NETMASK}:{orb}:{GUEST_INTERFACE}:off:{DNS}"),
        "--cpus".into(),
        format!("boot={}", settings.vcpus),
        "--memory".into(),
        format!("size={}M,thp=off", settings.memory_mib),
        // Root first, so it is /dev/vda; both are raw, which the VMM requires
        // to be said.
        "--disk".into(),
        format!("path={},image_type=raw", settings.disk_path(orb).display()),
        format!(
            "path={},readonly=on,image_type=raw",
            settings.config_disk_path(orb).display()
        ),
        "--net".into(),
        format!("tap={},ip={host},mask={NETMASK}", slot.tap()),
        "--balloon".into(),
        "size=0,free_page_reporting=on".into(),
        // Serial to stdout, which is the unit's journal.
        "--serial".into(),
        "tty".into(),
        "--console".into(),
        "off".into(),
    ]
}

/// Ok only when the orb's VMM says its VM is running; otherwise, why not.
pub async fn confirm_running(settings: &Settings, orb: &OrbId) -> Result<()> {
    let socket = settings.api_socket_path(orb);
    ensure!(socket.try_exists()?, "there is no API socket");

    let ch_remote = settings.cloud_hypervisor.with_file_name("ch-remote");
    let info = run(
        &ch_remote.display().to_string(),
        &[&format!("--api-socket={}", socket.display()), "info"],
    )
    .await?;
    let info: serde_json::Value = serde_json::from_str(&info).context("reading vm.info")?;
    ensure!(info["state"] == "Running", "the VM is {}", info["state"]);
    Ok(())
}

pub async fn wait_until_running(settings: &Settings, orb: &OrbId) -> Result<()> {
    let deadline = Instant::now() + limits::BOOT_WAIT;
    loop {
        match confirm_running(settings, orb).await {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => {
                return Err(error.context(format!("not running after {:?}", limits::BOOT_WAIT)));
            }
            Err(_) => sleep(limits::BOOT_POLL).await,
        }
    }
}

/// The policy only drops, so without its table an orb would reach the host
/// and the LAN and nothing else would notice.
pub async fn ensure_network_policy() -> Result<()> {
    let [family, name] = POLICY_TABLE;
    run("nft", &["list", "table", family, name])
        .await
        .with_context(|| format!("the network policy table {family} {name} is not loaded"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_settings;

    #[test]
    fn the_command_is_the_fixed_vm_shape() {
        let settings = test_settings("/var/lib/jorb", "/run/jorb");
        let command = command(&settings, &"amp-1".parse().unwrap(), Slot(7));

        assert_eq!(
            command.join(" "),
            "/bin/cloud-hypervisor --api-socket=/run/jorb/amp-1.sock \
             --kernel /var/lib/jorb/kernel \
             --cmdline console=ttyAMA0 root=/dev/vda rw init=/jorb-init \
             ip=10.77.7.2::10.77.7.1:255.255.255.252:amp-1:eth0:off:1.1.1.1:9.9.9.9 \
             --cpus boot=4 --memory size=6144M,thp=off \
             --disk path=/var/lib/jorb/disks/amp-1.raw,image_type=raw \
             path=/run/jorb/amp-1.cfg,readonly=on,image_type=raw \
             --net tap=jorb7,ip=10.77.7.1,mask=255.255.255.252 \
             --balloon size=0,free_page_reporting=on --serial tty --console off"
        );
    }
}
