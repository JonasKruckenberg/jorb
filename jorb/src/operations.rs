//! The operations Amp asks for, and the four primitives they are made of.
//! Every request runs `recover`, then its operation:
//!
//! | Operation | Composition |
//! |---|---|
//! | pause | `stop` |
//! | delete | `stop`, `remove_disk` |
//! | spawn | nothing if running with an identical config disk; otherwise `stop`, `ensure_disk`, `start` |
//!
//! "Running without a disk" must be unreachable: the disk is created before
//! the VM starts and the VM stopped before the disk goes. Each primitive
//! establishes its own preconditions and observes its postcondition. Nothing
//! cleans up on the way out; what a failed request leaves is the next one's
//! `recover`'s.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use jorb_config_disk::ConfigDisk;
use tracing::{info, instrument, warn};

use crate::request::{OrbId, Request};
use crate::settings::Settings;
use crate::systemd;
use crate::vm::{self, Slot};

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

pub async fn run(settings: &Settings, request: &Request) -> Result<()> {
    let running = recover(settings).await?;
    let orb = &request.orb;
    match &request.operation {
        Operation::Spawn(config_disk) => {
            if running.contains(orb)
                && current_config_disk(settings, orb).as_ref() == Some(config_disk)
            {
                info!("already running with this config");
                return Ok(());
            }
            stop(settings, orb).await?;
            ensure_disk(settings, orb)?;
            start(settings, orb, config_disk).await
        }
        Operation::Pause => stop(settings, orb).await,
        Operation::Delete => {
            stop(settings, orb).await?;
            remove_if_present(&settings.disk_path(orb))
        }
    }
}

/// What the orb's config disk says now. Unreadable counts as different, which
/// restarts the orb with a fresh one. A changed token leaves the old VM unable
/// to reach Amp, so only an equal config disk counts as healthy.
fn current_config_disk(settings: &Settings, orb: &OrbId) -> Option<ConfigDisk> {
    let file = File::open(settings.config_disk_path(orb)).ok()?;
    ConfigDisk::read_from(file).ok()
}

/// Stops every orb unit whose VM is not confirmed running, and returns the
/// orbs that are. Under the lock no request is mid-start, so such a unit is debris
/// or a VMM that has stopped answering; either way it holds a slot. The cost
/// is that a VMM slower than the subprocess timeout is stopped while alive.
///
/// Then it removes every config disk and socket that belongs to no running
/// orb: a guest that powered off takes its unit with it, not its files, and
/// a config disk holds a token.
#[instrument(skip_all)]
async fn recover(settings: &Settings) -> Result<Vec<OrbId>> {
    let mut running = Vec::new();
    for orb in systemd::orb_units()
        .await?
        .iter()
        .filter_map(|unit| systemd::orb_of(unit))
    {
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
            // `recovered`, because `orb` is the request's own field.
            Err(reason) => {
                warn!(recovered = %orb, reason = format!("{reason:#}"), "stopping");
                stop(settings, &orb).await?;
            }
        }
    }

    for entry in std::fs::read_dir(&settings.run_directory)? {
        let path = entry?.path();
        let owner = path
            .file_stem()
            .and_then(|stem| stem.to_str()?.parse::<OrbId>().ok());
        if !owner.is_some_and(|orb| running.contains(&orb)) {
            remove_if_present(&path)?;
        }
    }
    Ok(running)
}

/// The config disk goes too: it holds the token, needed only while running.
#[instrument(skip_all)]
async fn stop(settings: &Settings, orb: &OrbId) -> Result<()> {
    systemd::stop(orb).await?;
    remove_if_present(&settings.api_socket_path(orb))?;
    remove_if_present(&settings.config_disk_path(orb))
}

/// A reflink clone, written under the partial name, flushed and renamed, so a
/// kill leaves no disk or a whole one.
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

/// Nothing is undone if this fails; the unit, socket and tap it leaves are
/// recover's.
#[instrument(skip_all, fields(slot))]
async fn start(settings: &Settings, orb: &OrbId, config_disk: &ConfigDisk) -> Result<()> {
    vm::ensure_network_policy().await?;
    remove_if_present(&settings.api_socket_path(orb))?;
    let slot = Slot::lowest_free(settings)?;
    tracing::Span::current().record("slot", slot.index());

    // Mode 0600 on a tmpfs: the token never reaches a persistent disk.
    let config_path = settings.config_disk_path(orb);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&config_path)
        .with_context(|| format!("creating {}", config_path.display()))?;
    config_disk.write_to(&mut file)?;
    file.sync_all()?;

    let invocation = systemd::start(orb, &vm::vmm_command(settings, orb)).await?;
    info!(invocation, "started");
    vm::boot(settings, orb, slot)
        .await
        .with_context(|| format!("see journalctl _SYSTEMD_INVOCATION_ID={invocation}"))?;
    info!("running");
    Ok(())
}

/// Already gone is the same outcome, so every caller is safe to repeat.
fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(error).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}
