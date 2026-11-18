//! The model: every job runs `recover`, then its operation, and each
//! operation is a composition of four primitives.
//!
//! | Operation | Composition |
//! |---|---|
//! | pause | `stop` |
//! | delete | `stop`, `remove_disk` |
//! | spawn | no-op if already running with a byte-identical config disk; otherwise `stop`, `ensure_disk`, `start` |
//!
//! An orb's state is two facts, disk exists and VM running. "Running without
//! disk" must be unreachable, so the disk is created before the VM starts and
//! the VM is stopped before the disk is unlinked.
//!
//! Each primitive establishes its own preconditions on entry and verifies its
//! postcondition by observation. Nothing here cleans up on the way out: what a
//! killed or failed job leaves behind is the next job's `recover` to remove.

use std::fs::{File, OpenOptions};

use anyhow::{Context, Result, ensure};
use jorb_config_disk::ConfigDisk;
use jorb_vmm::VmState;
use tracing::{debug, info, instrument, warn};

use crate::disk;
use crate::job::OrbId;
use crate::settings::Settings;
use crate::unit;
use crate::vm::{self, Slot};

/// Stops every orb unit whose VM is not confirmed running, whichever orb it
/// belongs to, and returns the orbs that are.
///
/// Under the lock no job is mid-start, so such a unit is debris from a job
/// that died, and stopping it frees the slot its half-started VM held. A VMM
/// that does not answer is stopped too: leaving it would fail every job on
/// the host, including the pause or delete that would clear it. The cost is
/// that a VMM slower than the API timeout is stopped while alive.
#[instrument(skip_all)]
pub async fn recover(settings: &Settings) -> Result<Vec<OrbId>> {
    let units = unit::orb_units().await?;
    debug!(count = units.len(), "orb units");

    let mut running = Vec::new();
    for unit in units {
        let Some(orb) = unit::orb_of_unit(&unit) else {
            info!(unit = %unit, "a unit that is not named after an orb; leaving it alone");
            continue;
        };

        match vm::state(settings, &orb).await {
            Ok(VmState::Running) => {
                running.push(orb);
                continue;
            }
            Ok(state) => {
                info!(orb = %orb, state = ?state, "stopping a unit whose VM is not running");
            }
            Err(error) => warn!(
                orb = %orb,
                error = format!("{error:#}"),
                "stopping a unit whose VMM did not answer"
            ),
        }
        stop(settings, &orb).await?;
    }

    Ok(running)
}

/// Leaves a healthy orb alone, otherwise starts it from where it is.
///
/// Healthy means a running VM with a byte-identical config disk. A changed
/// token leaves the old VM unable to talk to Amp, so it is restarted with the
/// new one. `running` is what this job's `recover` confirmed, under the same
/// lock.
#[instrument(skip_all)]
pub async fn spawn(
    settings: &Settings,
    orb: &OrbId,
    config_disk: &ConfigDisk,
    running: &[OrbId],
) -> Result<()> {
    let config_disk_path = disk::config_disk_path(settings, orb);
    if running.contains(orb) && disk::config_disk_matches(&config_disk_path, config_disk)? {
        info!("already running with this config; leaving it alone");
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

/// Stops the orb's unit and takes its socket away.
#[instrument(skip_all, fields(orb = %orb))]
async fn stop(settings: &Settings, orb: &OrbId) -> Result<()> {
    unit::stop(orb).await?;
    disk::remove_if_present(&vm::api_socket_path(settings, orb))
}

/// Gives the orb a disk if it has none, by cloning the base image.
///
/// The clone is written to the partial name and renamed, so a kill in the
/// middle leaves either no disk or a whole one. The partial name is truncated
/// on entry rather than trusted to be absent.
#[instrument(skip_all, fields(orb = %orb))]
fn ensure_disk(settings: &Settings, orb: &OrbId) -> Result<()> {
    let disk = disk::disk_path(settings, orb);
    if disk.try_exists()? {
        debug!("the disk is already there");
        return Ok(());
    }

    let image = disk::base_image_path(settings);
    let base = File::open(&image).with_context(|| {
        format!(
            "opening the base image at {}; has jorb-prepare-image run?",
            image.display()
        )
    })?;

    let partial_path = disk::partial_disk_path(settings);
    let partial = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&partial_path)
        .with_context(|| format!("creating {}", partial_path.display()))?;

    // A reflink, so a 64 GiB disk costs what the orb writes to it.
    rustix::fs::ioctl_ficlone(&partial, &base)
        .with_context(|| format!("reflinking the base image to {}", partial_path.display()))?;
    partial
        .sync_all()
        .with_context(|| format!("flushing {}", partial_path.display()))?;
    drop(partial);

    std::fs::rename(&partial_path, &disk)
        .with_context(|| format!("renaming the partial disk to {}", disk.display()))?;

    // The rename has to survive a power cut too, which means fsyncing the
    // directory that holds the name.
    let directory = disk::disks_directory(settings);
    rustix::fs::fsync(&File::open(&directory)?)
        .with_context(|| format!("flushing {}", directory.display()))?;

    ensure!(
        disk.try_exists()?,
        "the disk at {} is not there after being created",
        disk.display()
    );
    info!(disk = %disk.display(), "disk cloned from the base image");

    Ok(())
}

/// Takes the orb's disk and its config disk away.
#[instrument(skip_all, fields(orb = %orb))]
fn remove_disk(settings: &Settings, orb: &OrbId) -> Result<()> {
    let disk = disk::disk_path(settings, orb);

    disk::remove_if_present(&disk)?;
    disk::remove_if_present(&disk::config_disk_path(settings, orb))?;

    ensure!(
        !disk.try_exists()?,
        "the disk at {} is still there after being removed",
        disk.display()
    );

    Ok(())
}

/// Starts the orb: config disk, slot, unit, VM. Nothing is undone if any of
/// this fails; the unit, socket and tap left behind are recover's.
#[instrument(skip_all, fields(orb = %orb, slot = tracing::field::Empty))]
async fn start(settings: &Settings, orb: &OrbId, config_disk: &ConfigDisk) -> Result<()> {
    disk::remove_if_present(&vm::api_socket_path(settings, orb))?;
    disk::write_config_disk(&disk::config_disk_path(settings, orb), config_disk)?;

    let slot = Slot::lowest_free(settings)?;
    tracing::Span::current().record("slot", slot.index());

    let unit = unit::start(orb, &vm::vmm_command(settings, orb)).await?;
    info!(slot = slot.index(), unit = %unit, "starting");

    vm::boot(settings, orb, slot).await?;
    info!(slot = slot.index(), "running");

    Ok(())
}
