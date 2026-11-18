//! The model: what each operation Amp asks for does to an orb. An orb is two
//! facts, *disk exists* and *VM running*, and every request first clears the
//! dead orbs, then applies its operation:
//!
//! | Operation | Steps |
//! |---|---|
//! | pause | `vm::power_off` |
//! | delete | `vm::power_off`, remove the disk |
//! | spawn | nothing if running with an equal config disk; otherwise `vm::power_off`, `ensure_disk`, `vm::boot` |
//!
//! "Running without a disk" is unreachable: the disk exists before the VM
//! boots and the VM is off before the disk goes. Each step establishes its own
//! preconditions, and nothing cleans up on the way out; what a failed request
//! leaves is the next one's to clear.

use std::fs::File;

use anyhow::{Context, Result, ensure};
use jorb_config_disk::ConfigDisk;
use tracing::{info, instrument, warn};

use crate::request::{OrbId, Request};
use crate::settings::Settings;
use crate::{systemd, vm};

/// Only a spawn carries a config disk, so a spawn without a token cannot be
/// represented. The config disk's `Debug` omits the token.
#[derive(Debug)]
pub enum Operation {
    Spawn(ConfigDisk),
    Pause,
    Delete,
}

impl Operation {
    /// As Amp spells it, for the `op` log field.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Spawn(_) => "spawn",
            Self::Pause => "pause",
            Self::Delete => "delete",
        }
    }
}

/// Drives the request's orb to the state its operation asks for.
pub async fn apply(settings: &Settings, request: &Request) -> Result<()> {
    let running = clear_dead_orbs(settings).await?;
    let orb = &request.orb;
    match &request.operation {
        Operation::Spawn(config_disk) => {
            // A changed token leaves the old VM unable to reach Amp, so only an
            // equal config disk counts as healthy.
            let current = File::open(settings.config_disk_path(orb)).ok();
            if running.contains(orb)
                && current
                    .and_then(|file| ConfigDisk::read_from(file).ok())
                    .as_ref()
                    == Some(config_disk)
            {
                info!("already running with this config");
                return Ok(());
            }
            vm::power_off(settings, orb).await?;
            ensure_disk(settings, orb)?;
            vm::boot(settings, orb, config_disk).await
        }
        Operation::Pause => vm::power_off(settings, orb).await,
        Operation::Delete => {
            vm::power_off(settings, orb).await?;
            vm::remove_if_present(&settings.disk_path(orb))
        }
    }
}

/// Powers off every orb whose VM its VMM does not confirm running, and
/// returns the orbs that are. Under the lock no request is mid-boot, so such
/// an orb is debris from a request that died, or a VMM that stopped
/// answering; either way it holds a slot. A VMM slower than the API timeout
/// is powered off while alive.
#[instrument(skip_all)]
async fn clear_dead_orbs(settings: &Settings) -> Result<Vec<OrbId>> {
    let mut running = Vec::new();
    for orb in systemd::orbs().await? {
        match jorb_vmm::running(&settings.api_socket_path(&orb)).await {
            Ok(()) => {
                let disk = settings.disk_path(&orb);
                ensure!(
                    disk.try_exists()?,
                    "{orb} is running without its disk {}",
                    disk.display()
                );
                running.push(orb);
            }
            // `cleared`, because `orb` is the request's own field.
            Err(reason) => {
                warn!(cleared = %orb, reason = format!("{reason:#}"), "powering off");
                vm::power_off(settings, &orb).await?;
            }
        }
    }
    vm::remove_stray_files(settings, &running)?;
    Ok(running)
}

/// Clones the base image to the orb's disk if it has none: under the partial
/// name, flushed and renamed, so a kill leaves no disk or a whole one. A
/// reflink, so a 64 GiB disk costs what the orb writes.
#[instrument(skip_all)]
fn ensure_disk(settings: &Settings, orb: &OrbId) -> Result<()> {
    let disk = settings.disk_path(orb);
    if disk.try_exists()? {
        return Ok(());
    }
    let base_path = settings.base_image_path();
    let base =
        File::open(&base_path).with_context(|| format!("opening {}", base_path.display()))?;
    let partial_path = settings.partial_disk_path();
    let partial = File::create(&partial_path)
        .with_context(|| format!("creating {}", partial_path.display()))?;
    rustix::fs::ioctl_ficlone(&partial, &base)
        .with_context(|| format!("reflinking {}", base_path.display()))?;
    partial.sync_all()?;
    std::fs::rename(&partial_path, &disk)
        .with_context(|| format!("renaming to {}", disk.display()))?;
    File::open(settings.disks_directory())?.sync_all()?;
    info!(disk = %disk.display(), "disk cloned");
    Ok(())
}
