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
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use jorb_vmm::{VmBuilder, VmState, Vmm};
use tokio::time::sleep;
use tracing::{debug, info, instrument};

use crate::constants;
use crate::host::{HostConfig, Slot};
use jorb_config_disk::ConfigDisk;

use crate::job::OrbId;
use crate::systemd;

/// One orb on one host. Cheap to make: it is a pair of borrows, and holding
/// one says nothing about whether the orb exists.
#[derive(Debug, Clone, Copy)]
pub struct Orb<'a> {
    host: &'a HostConfig,
    id: &'a OrbId,
}

impl<'a> Orb<'a> {
    pub fn new(host: &'a HostConfig, id: &'a OrbId) -> Self {
        Self { host, id }
    }

    /// This orb's disk: a reflink clone of the base image, kept across a
    /// pause and removed by a delete.
    pub fn disk(&self) -> PathBuf {
        self.host.disks_directory().join(format!("{}.raw", self.id))
    }

    /// This orb's config disk, which is the whole of what the host tells it.
    pub fn config_disk(&self) -> PathBuf {
        self.host.run_directory().join(format!("{}.cfg", self.id))
    }

    /// The socket this orb's VMM listens on.
    pub fn api_socket(&self) -> PathBuf {
        self.host.run_directory().join(format!("{}.sock", self.id))
    }

    /// What the VMM behind this orb's socket says about it.
    pub async fn state(&self) -> Result<VmState> {
        Ok(Vmm::at(&self.api_socket())?.state().await?)
    }

    /// Whether the config disk on this host already says exactly this.
    ///
    /// Byte equality. The config disk has a fixed size so that this works.
    /// The contents are never logged: one of the values is the token.
    pub fn config_disk_matches(&self, wanted: &ConfigDisk) -> Result<bool> {
        let path = self.config_disk();

        match std::fs::read(&path) {
            Ok(existing) => Ok(existing == wanted.as_bytes()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Stops the orb's unit and takes its socket away.
    #[instrument(skip_all, fields(orb = %self.id))]
    pub async fn stop(&self) -> Result<()> {
        systemd::stop_orb_unit(self.id).await?;
        remove_if_present(&self.api_socket())?;

        Ok(())
    }

    /// Gives the orb a disk if it has none, by cloning the base image.
    ///
    /// The clone is written to a partial name and renamed, so a kill in the
    /// middle leaves either no disk or a whole one. The partial name is
    /// truncated on entry instead of being trusted to be absent.
    #[instrument(skip_all, fields(orb = %self.id))]
    pub async fn ensure_disk(&self) -> Result<()> {
        let disk = self.disk();
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
        let disk = self.disk();

        remove_if_present(&disk)?;
        remove_if_present(&self.config_disk())?;

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
        let socket = self.api_socket();
        let config_disk_path = self.config_disk();

        // Preconditions this primitive establishes for itself.
        remove_if_present(&socket)?;
        write_config_disk(&config_disk_path, config_disk)?;

        let slot = Slot::lowest_free(self.host)?;
        tracing::Span::current().record("slot", slot.index());

        let unit = systemd::start_orb_unit(self.host, self.id, &socket).await?;
        info!(slot = slot.index(), unit = %unit, "starting");

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
        .disk(&self.disk())
        .read_only_disk(&config_disk_path)
        .tap(
            &slot.tap_name(),
            slot.host_address(),
            constants::GUEST_NETMASK,
        )
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
    let deadline = Instant::now() + constants::API_SOCKET_WAIT;

    while !socket.exists() {
        ensure!(
            Instant::now() < deadline,
            "{} did not appear within {:?}",
            socket.display(),
            constants::API_SOCKET_WAIT
        );
        sleep(constants::API_SOCKET_POLL).await;
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
    use super::*;
    use crate::test_environment;

    /// A host whose directories are this test's own, so nothing it writes
    /// lands in /var/lib/jorb or /run/jorb.
    fn test_host(name: &str) -> HostConfig {
        let directory = std::env::temp_dir().join("jorb-tests").join(name);
        std::fs::create_dir_all(&directory).unwrap();

        let directory = directory.to_str().unwrap();
        HostConfig::from_environment(&test_environment(&[
            ("JORB_KERNEL", "/var/lib/jorb/kernel"),
            ("JORB_STATE_DIR", directory),
            ("JORB_RUN_DIR", directory),
        ]))
        .expect("the host parses")
    }

    #[test]
    fn a_config_disk_matches_only_its_own_bytes() {
        let host = test_host("config-disk");
        let id: OrbId = "amp-1".parse().unwrap();
        let orb = Orb::new(&host, &id);

        let wanted = crate::job::config_disk(&"a-token".into(), None).unwrap();
        let other = crate::job::config_disk(&"another-token".into(), None).unwrap();

        remove_if_present(&orb.config_disk()).unwrap();
        assert!(
            !orb.config_disk_matches(&wanted).unwrap(),
            "a config disk that is not there cannot match"
        );

        write_config_disk(&orb.config_disk(), &wanted).unwrap();

        assert!(orb.config_disk_matches(&wanted).unwrap());
        assert!(!orb.config_disk_matches(&other).unwrap());
    }

    #[test]
    fn removing_a_path_that_is_not_there_is_not_a_failure() {
        let host = test_host("removal");

        let id: OrbId = "not-here".parse().unwrap();

        assert!(remove_if_present(&Orb::new(&host, &id).disk()).is_ok());
    }
}
