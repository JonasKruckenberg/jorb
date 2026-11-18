//! One orb on this host, and the four primitives that change its state.
//!
//! An orb is a host and an id, so these are methods on the pair. Its state is two facts, disk exists and VM running;
//! "running without disk" must be unreachable, so the disk is created before
//! the VM starts and the VM is stopped before the disk is unlinked.
//!
//! Each primitive establishes its own preconditions on entry and verifies its
//! own postcondition by observation: the VMM's own answer, a file, an
//! interface. None trusts another's exit code, and none expects a predecessor
//! to have left things tidy. Nothing here cleans up on the way out; whatever a
//! killed job left behind belongs to the next job's recover.

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use jorb_vmm::{VmBuilder, VmState, Vmm};
use tokio::time::sleep;
use tracing::{debug, info, instrument};

use crate::fixed;
use crate::host::{self, Host};
use crate::request::{ConfigDisk, OrbId};

/// One orb on one host. Cheap to make: it is a pair of borrows, and holding
/// one says nothing about whether the orb exists.
#[derive(Debug, Clone, Copy)]
pub struct Orb<'a> {
    host: &'a Host,
    id: &'a OrbId,
}

impl<'a> Orb<'a> {
    pub fn new(host: &'a Host, id: &'a OrbId) -> Self {
        Self { host, id }
    }

    /// What the VMM behind this orb's socket says about it.
    pub async fn state(&self) -> Result<VmState> {
        Ok(Vmm::at(&self.host.api_socket(self.id))?.state().await?)
    }

    /// Whether the config disk on this host already says exactly this.
    ///
    /// Byte equality. The config disk has a fixed size so that this works.
    /// The contents are never logged: one of the values is the token.
    pub fn config_disk_matches(&self, wanted: &ConfigDisk) -> Result<bool> {
        let path = self.host.config_disk(self.id);

        match std::fs::read(&path) {
            Ok(existing) => Ok(existing == wanted.as_bytes()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Stops the orb's unit and takes its socket away.
    ///
    /// The postcondition is systemd's own answer, not the exit code of the
    /// stop. A unit that was already gone and a unit that had to be killed
    /// look the same from here, so repeating this is safe.
    #[instrument(skip_all, fields(orb = %self.id))]
    pub async fn stop(&self) -> Result<()> {
        let unit = host::unit_name(self.id);

        // --no-block would leave the postcondition to a race; this waits.
        let _ = host::run(fixed::SYSTEMCTL, &["--no-ask-password", "stop", &unit]).await;

        let deadline = Instant::now() + fixed::UNIT_GONE_WAIT;
        while host::unit_is_active(&unit).await? {
            ensure!(
                Instant::now() < deadline,
                "{unit} was still active {:?} after being stopped",
                fixed::UNIT_GONE_WAIT
            );
            sleep(fixed::UNIT_GONE_POLL).await;
        }

        remove_if_present(&self.host.api_socket(self.id))?;

        Ok(())
    }

    /// Gives the orb a disk if it has none, by cloning the base image.
    ///
    /// The clone is written to a partial name and renamed, so a kill in the
    /// middle leaves either no disk or a whole one. The partial name is
    /// truncated on entry instead of being trusted to be absent.
    #[instrument(skip_all, fields(orb = %self.id))]
    pub async fn ensure_disk(&self) -> Result<()> {
        let disk = self.host.disk(self.id);
        if disk.try_exists()? {
            debug!("the disk is already there");
            return Ok(());
        }

        let image = self.host.base_image();
        let base = File::open(&image).with_context(|| {
            format!(
                "opening the base image at {}; has jorb-prepare-image run?",
                image.display()
            )
        })?;

        let partial_path = self.host.partial_disk();
        let partial = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&partial_path)
            .with_context(|| format!("creating {}", partial_path.display()))?;

        // A reflink, so a 64 GiB disk costs what the orb writes to it. It
        // needs XFS; preflight checks for that.
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
        let directory = self.host.disks_directory();
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

    /// Takes the orb's disk and its config disk away. Repeating this is a
    /// no-op.
    #[instrument(skip_all, fields(orb = %self.id))]
    pub async fn remove_disk(&self) -> Result<()> {
        let disk = self.host.disk(self.id);

        remove_if_present(&disk)?;
        remove_if_present(&self.host.config_disk(self.id))?;

        ensure!(
            !disk.try_exists()?,
            "the disk at {} is still there after being removed",
            disk.display()
        );

        Ok(())
    }

    /// Starts the orb: config disk, slot, unit, VM.
    ///
    /// Nothing is undone if any of this fails. The unit, the socket and the
    /// tap left behind are what the next job's recover is for, and tidying up
    /// here would be a second path that a kill skips anyway.
    #[instrument(skip_all, fields(orb = %self.id, slot = tracing::field::Empty))]
    pub async fn start(&self, config_disk: &ConfigDisk) -> Result<()> {
        let socket = self.host.api_socket(self.id);
        let config_disk_path = self.host.config_disk(self.id);

        // Preconditions this primitive establishes for itself.
        remove_if_present(&socket)?;
        write_config_disk(&config_disk_path, config_disk)?;

        let slot = self.host.lowest_free_slot()?;
        let unit = host::unit_name(self.id);
        tracing::Span::current().record("slot", slot.index());
        info!(slot = slot.index(), unit = %unit, "starting");

        host::run(
            fixed::SYSTEMD_RUN,
            &[
                "--no-ask-password",
                "--collect",
                &format!("--unit={unit}"),
                &format!("--slice={}", fixed::SLICE),
                "--property=Type=exec",
                "--property=OOMScoreAdjust=500",
                &format!(
                    "--property=TimeoutStopSec={}",
                    fixed::UNIT_STOP_TIMEOUT.as_secs()
                ),
                "--",
                &self.host.cloud_hypervisor.to_string_lossy(),
                &format!("--api-socket={}", socket.display()),
            ],
        )
        .await
        .with_context(|| format!("starting {unit}"))?;

        wait_for_socket(&socket).await?;

        // boot confirms the VM is running before it returns, so that is this
        // primitive's postcondition met. The handle is dropped here: a jorb
        // process ends while the VM it started keeps running.
        VmBuilder::new(
            &self.host.kernel,
            &slot.cmdline(self.id),
            self.host.vcpus,
            self.host.memory_mib,
        )
        .without_huge_pages()
        .disk(&self.host.disk(self.id))
        .read_only_disk(&config_disk_path)
        .tap(&slot.tap_name(), slot.host_address(), fixed::GUEST_NETMASK)
        .free_page_reporting()
        .boot(&Vmm::at(&socket)?)
        .await
        .context("creating and booting the VM")?;

        info!(slot = slot.index(), "running");

        Ok(())
    }
}

/// The config disk, written where only root can read it. The run directory is
/// a tmpfs, so the token never reaches a disk that survives a reboot.
fn write_config_disk(path: &Path, config_disk: &ConfigDisk) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;

    file.write_all(config_disk.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flushing {}", path.display()))?;

    Ok(())
}

/// Waits for Cloud Hypervisor to open its API socket.
async fn wait_for_socket(socket: &Path) -> Result<()> {
    let deadline = Instant::now() + fixed::API_SOCKET_WAIT;

    while !socket.exists() {
        ensure!(
            Instant::now() < deadline,
            "{} did not appear within {:?}",
            socket.display(),
            fixed::API_SOCKET_WAIT
        );
        sleep(fixed::API_SOCKET_POLL).await;
    }

    Ok(())
}

/// Removes a path if it is there. A path that was already gone is the same
/// outcome, so every primitive using this is safe to repeat.
fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Only {
        #[command(flatten)]
        host: Host,
    }

    /// A host whose directories are this test's own, so nothing it writes
    /// lands in /var/lib/jorb or /run/jorb.
    fn test_host(name: &str) -> Host {
        let directory = std::env::temp_dir().join("jorb-tests").join(name);
        std::fs::create_dir_all(&directory).unwrap();

        Only::try_parse_from([
            "jorb",
            "--kernel",
            "/var/lib/jorb/kernel",
            "--state-dir",
            directory.to_str().unwrap(),
            "--run-dir",
            directory.to_str().unwrap(),
        ])
        .expect("the host parses")
        .host
    }

    #[test]
    fn a_config_disk_matches_only_its_own_bytes() {
        let host = test_host("config-disk");
        let id: OrbId = "amp-1".parse().unwrap();
        let orb = Orb::new(&host, &id);

        let wanted = ConfigDisk::new(Some(&"a-token".into()), None).unwrap();
        let other = ConfigDisk::new(Some(&"another-token".into()), None).unwrap();

        remove_if_present(&host.config_disk(&id)).unwrap();
        assert!(
            !orb.config_disk_matches(&wanted).unwrap(),
            "a config disk that is not there cannot match"
        );

        write_config_disk(&host.config_disk(&id), &wanted).unwrap();

        assert!(orb.config_disk_matches(&wanted).unwrap());
        assert!(!orb.config_disk_matches(&other).unwrap());
    }

    #[test]
    fn removing_a_path_that_is_not_there_is_not_a_failure() {
        let host = test_host("removal");

        assert!(remove_if_present(&host.disk(&"not-here".parse().unwrap())).is_ok());
    }
}
