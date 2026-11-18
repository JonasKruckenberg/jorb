//! Where an orb's files live, and the file operations the primitives share.
//!
//! The state directory holds the base image, the lock and `disks/`, where
//! each orb's disk is `<id>.raw` and a clone in progress is `.tmp`. The run
//! directory holds each orb's config disk `<id>.cfg`; its API socket is the
//! VM's business and lives in `vm.rs`.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jorb_config_disk::ConfigDisk;

use crate::job::OrbId;
use crate::settings::Settings;

const BASE_IMAGE: &str = "base.raw";
const LOCK: &str = "lock";
const DISKS_DIRECTORY: &str = "disks";

/// One name for the whole host, because one job runs at a time.
const PARTIAL_DISK: &str = ".tmp";

/// The config disk holds the token, so only root reads it.
const CONFIG_DISK_MODE: u32 = 0o600;

/// The ext4 image every orb's disk is cloned from.
pub fn base_image_path(settings: &Settings) -> PathBuf {
    settings.state_directory.join(BASE_IMAGE)
}

/// The one lock, held for the whole of every job.
pub fn lock_path(settings: &Settings) -> PathBuf {
    settings.state_directory.join(LOCK)
}

/// The disk and the partial disk share this directory, so the rename between
/// them is atomic.
pub fn disks_directory(settings: &Settings) -> PathBuf {
    settings.state_directory.join(DISKS_DIRECTORY)
}

pub fn partial_disk_path(settings: &Settings) -> PathBuf {
    disks_directory(settings).join(PARTIAL_DISK)
}

/// A reflink clone of the base image, kept across a pause and removed by a
/// delete.
pub fn disk_path(settings: &Settings, orb: &OrbId) -> PathBuf {
    disks_directory(settings).join(format!("{orb}.raw"))
}

pub fn config_disk_path(settings: &Settings, orb: &OrbId) -> PathBuf {
    settings.run_directory.join(format!("{orb}.cfg"))
}

/// Whether the config disk on this host already holds exactly these bytes. A
/// missing one does not.
pub fn config_disk_matches(path: &Path, wanted: &ConfigDisk) -> Result<bool> {
    match std::fs::read(path) {
        Ok(existing) => Ok(existing == wanted.as_bytes()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Overwrites the config disk. The run directory is a tmpfs, so the token
/// never reaches a disk that survives a reboot.
pub fn write_config_disk(path: &Path, config_disk: &ConfigDisk) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(CONFIG_DISK_MODE)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;

    file.write_all(config_disk.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flushing {}", path.display()))
}

/// Removes a path if it is there. A path that was already gone is the same
/// outcome, so every primitive using this is safe to repeat.
pub fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_environment;

    /// Settings whose directories are this test's own.
    fn test_settings(name: &str) -> Settings {
        let directory = std::env::temp_dir().join("jorb-tests").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        let directory = directory.to_str().unwrap();

        Settings::from_environment(&test_environment(&[
            ("JORB_KERNEL", "/var/lib/jorb/kernel"),
            ("JORB_STATE_DIR", directory),
            ("JORB_RUN_DIR", directory),
        ]))
        .unwrap()
    }

    #[test]
    fn the_layout_is_the_fixed_interface() {
        let settings = Settings::from_environment(&test_environment(&[(
            "JORB_KERNEL",
            "/var/lib/jorb/kernel",
        )]))
        .unwrap();
        let orb: OrbId = "amp-1".parse().unwrap();

        assert_eq!(
            base_image_path(&settings),
            Path::new("/var/lib/jorb/base.raw")
        );
        assert_eq!(lock_path(&settings), Path::new("/var/lib/jorb/lock"));
        assert_eq!(
            partial_disk_path(&settings),
            Path::new("/var/lib/jorb/disks/.tmp")
        );
        assert_eq!(
            disk_path(&settings, &orb),
            Path::new("/var/lib/jorb/disks/amp-1.raw")
        );
        assert_eq!(
            config_disk_path(&settings, &orb),
            Path::new("/run/jorb/amp-1.cfg")
        );
    }

    #[test]
    fn a_config_disk_matches_only_its_own_bytes() {
        let settings = test_settings("config-disk");
        let path = config_disk_path(&settings, &"amp-1".parse().unwrap());

        let wanted = crate::job::config_disk(&"a-token".into(), None).unwrap();
        let other = crate::job::config_disk(&"another-token".into(), None).unwrap();

        remove_if_present(&path).unwrap();
        assert!(
            !config_disk_matches(&path, &wanted).unwrap(),
            "a config disk that is not there cannot match"
        );

        write_config_disk(&path, &wanted).unwrap();

        assert!(config_disk_matches(&path, &wanted).unwrap());
        assert!(!config_disk_matches(&path, &other).unwrap());
        remove_if_present(&path).unwrap();
        remove_if_present(&path).unwrap();
    }
}
