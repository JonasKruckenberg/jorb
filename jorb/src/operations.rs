//! The model. Every job runs `recover`, then its operation:
//!
//! | Operation | Composition |
//! |---|---|
//! | pause | `stop` |
//! | delete | `stop`, `remove_disk` |
//! | spawn | nothing if running with a byte-identical config disk; otherwise `stop`, `ensure_disk`, `start` |
//!
//! "Running without a disk" must be unreachable: the disk is created before
//! the VM starts and the VM stopped before the disk goes. Each primitive
//! establishes its own preconditions and observes its postcondition. Nothing
//! cleans up on the way out; what a failed job leaves is the next one's
//! `recover`'s.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use jorb_config_disk::ConfigDisk;
use tracing::{info, instrument, warn};

use crate::job::OrbId;
use crate::settings::Settings;
use crate::unit;
use crate::vm::{self, Slot};

/// Stops every orb unit whose VM is not confirmed running, and returns the
/// orbs that are. Under the lock no job is mid-start, so such a unit is debris
/// or a VMM that has stopped answering; either way it holds a slot. The cost
/// is that a VMM slower than the subprocess timeout is stopped while alive.
///
/// Then it removes every config disk and socket that belongs to no running
/// orb: a guest that powered off takes its unit with it, not its files, and
/// a config disk holds a token.
#[instrument(skip_all)]
pub async fn recover(settings: &Settings) -> Result<Vec<OrbId>> {
    let mut running = Vec::new();
    for orb in unit::orb_units()
        .await?
        .iter()
        .filter_map(|unit| unit::orb_of(unit))
    {
        match vm::confirm_running(settings, &orb).await {
            Ok(()) => {
                let disk = settings.disk_path(&orb);
                ensure!(
                    disk.try_exists()?,
                    "{orb} is running without its disk {}",
                    disk.display()
                );
                running.push(orb);
            }
            // `recovered`, because `orb` is the job's own field.
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

/// A changed token leaves the old VM unable to reach Amp, so only a
/// byte-identical config disk counts as healthy.
pub async fn spawn(
    settings: &Settings,
    orb: &OrbId,
    config_disk: &ConfigDisk,
    running: &[OrbId],
) -> Result<()> {
    let current = std::fs::read(settings.config_disk_path(orb)).ok();
    if running.contains(orb) && current.as_deref() == Some(config_disk.as_bytes()) {
        info!("already running with this config");
        return Ok(());
    }
    stop(settings, orb).await?;
    ensure_disk(settings, orb)?;
    start(settings, orb, config_disk).await
}

pub async fn pause(settings: &Settings, orb: &OrbId) -> Result<()> {
    stop(settings, orb).await
}

pub async fn delete(settings: &Settings, orb: &OrbId) -> Result<()> {
    stop(settings, orb).await?;
    remove_disk(settings, orb)
}

/// The config disk goes too: it holds the token, needed only while running.
#[instrument(skip_all)]
async fn stop(settings: &Settings, orb: &OrbId) -> Result<()> {
    unit::stop(orb).await?;
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

    ensure!(
        disk.try_exists()?,
        "{} is missing after being created",
        disk.display()
    );
    info!(disk = %disk.display(), "disk cloned");
    Ok(())
}

#[instrument(skip_all)]
fn remove_disk(settings: &Settings, orb: &OrbId) -> Result<()> {
    let disk = settings.disk_path(orb);
    remove_if_present(&disk)?;
    ensure!(
        !disk.try_exists()?,
        "{} is still there after being removed",
        disk.display()
    );
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
    file.write_all(config_disk.as_bytes())?;
    file.sync_all()?;

    let invocation = unit::start(orb, &vm::command(settings, orb, slot)).await?;
    info!(invocation, "started");
    vm::wait_until_running(settings, orb)
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
