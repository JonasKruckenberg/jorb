//! What this host is, from `JORB_*` variables the root-side wrapper sets, and
//! where its files are. None of it is the caller's to choose.
//!
//! The state directory holds the base image, the lock and `disks/<id>.raw`;
//! the run directory, a tmpfs, holds each orb's config disk and API socket.

use std::num::{NonZeroU8, NonZeroU32};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};

use crate::job::OrbId;
use crate::{Environment, limits, required};

#[derive(Debug)]
pub struct Settings {
    /// `1..=MAX_SLOTS`.
    pub slots: u16,
    pub vcpus: NonZeroU8,
    pub memory_mib: NonZeroU32,
    pub kernel: PathBuf,
    pub state_directory: PathBuf,
    pub run_directory: PathBuf,
    pub cloud_hypervisor: PathBuf,
}

impl Settings {
    pub fn from_environment(environment: Environment) -> Result<Self> {
        let settings = Self {
            slots: parsed(environment, "JORB_SLOTS")?,
            vcpus: parsed(environment, "JORB_VCPUS")?,
            memory_mib: parsed(environment, "JORB_MEMORY_MIB")?,
            kernel: parsed(environment, "JORB_KERNEL")?,
            state_directory: parsed(environment, "JORB_STATE_DIR")?,
            run_directory: parsed(environment, "JORB_RUN_DIR")?,
            cloud_hypervisor: parsed(environment, "JORB_CH_BIN")?,
        };
        ensure!(
            (1..=limits::MAX_SLOTS).contains(&settings.slots),
            "JORB_SLOTS must be between 1 and {}",
            limits::MAX_SLOTS
        );
        Ok(settings)
    }

    pub fn base_image_path(&self) -> PathBuf {
        self.state_directory.join("base.raw")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.state_directory.join("lock")
    }

    /// Disks and the partial disk share a directory, so the rename between
    /// them is atomic.
    pub fn disks_directory(&self) -> PathBuf {
        self.state_directory.join("disks")
    }

    /// One name for the host, because one job runs at a time.
    pub fn partial_disk_path(&self) -> PathBuf {
        self.disks_directory().join(".tmp")
    }

    pub fn disk_path(&self, orb: &OrbId) -> PathBuf {
        self.disks_directory().join(format!("{orb}.raw"))
    }

    pub fn config_disk_path(&self, orb: &OrbId) -> PathBuf {
        self.run_directory.join(format!("{orb}.cfg"))
    }

    pub fn api_socket_path(&self, orb: &OrbId) -> PathBuf {
        self.run_directory.join(format!("{orb}.sock"))
    }
}

fn parsed<T>(environment: Environment, name: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let raw = required(environment, name)?;
    raw.parse().with_context(|| format!("{name} is {raw:?}"))
}

#[cfg(test)]
pub fn test_settings(state_directory: &str, run_directory: &str) -> Settings {
    Settings::from_environment(&crate::test_environment(&[
        ("JORB_SLOTS", "1"),
        ("JORB_VCPUS", "4"),
        ("JORB_MEMORY_MIB", "6144"),
        ("JORB_KERNEL", "/var/lib/jorb/kernel"),
        ("JORB_STATE_DIR", state_directory),
        ("JORB_RUN_DIR", run_directory),
        ("JORB_CH_BIN", "/bin/cloud-hypervisor"),
    ]))
    .unwrap()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn the_layout_is_the_fixed_interface() {
        let settings = test_settings("/var/lib/jorb", "/run/jorb");
        let orb: OrbId = "amp-1".parse().unwrap();

        for (path, expected) in [
            (settings.base_image_path(), "/var/lib/jorb/base.raw"),
            (settings.lock_path(), "/var/lib/jorb/lock"),
            (settings.partial_disk_path(), "/var/lib/jorb/disks/.tmp"),
            (settings.disk_path(&orb), "/var/lib/jorb/disks/amp-1.raw"),
            (settings.config_disk_path(&orb), "/run/jorb/amp-1.cfg"),
            (settings.api_socket_path(&orb), "/run/jorb/amp-1.sock"),
        ] {
            assert_eq!(path, Path::new(expected));
        }
    }

    #[test]
    fn every_setting_is_required_and_checked() {
        assert!(Settings::from_environment(&crate::test_environment(&[])).is_err());

        for (name, value) in [
            ("JORB_SLOTS", "0"),
            ("JORB_SLOTS", "257"),
            ("JORB_VCPUS", "0"),
        ] {
            let mut variables = vec![
                ("JORB_SLOTS", "1"),
                ("JORB_VCPUS", "4"),
                ("JORB_MEMORY_MIB", "6144"),
                ("JORB_KERNEL", "/k"),
                ("JORB_STATE_DIR", "/s"),
                ("JORB_RUN_DIR", "/r"),
                ("JORB_CH_BIN", "/c"),
            ];
            variables.retain(|(existing, _)| *existing != name);
            variables.push((name, value));
            assert!(
                Settings::from_environment(&crate::test_environment(&variables)).is_err(),
                "{name}={value}"
            );
        }
    }
}
