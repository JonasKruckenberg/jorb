//! What this host is, from `JORB_*` variables the root-side wrapper sets, and
//! where its files are. None of it is the caller's to choose.
//!
//! The state directory holds the base image, the lock and `disks/<id>.raw`;
//! the run directory, a tmpfs, holds each orb's config disk and API socket.

use std::collections::HashMap;
use std::num::{NonZeroU8, NonZeroU32};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};

use crate::request::OrbId;
use crate::{limits, required};

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
    pub fn parse(variables: &HashMap<String, String>) -> Result<Self> {
        let settings = Self {
            slots: parsed(variables, "JORB_SLOTS")?,
            vcpus: parsed(variables, "JORB_VCPUS")?,
            memory_mib: parsed(variables, "JORB_MEMORY_MIB")?,
            kernel: parsed(variables, "JORB_KERNEL")?,
            state_directory: parsed(variables, "JORB_STATE_DIR")?,
            run_directory: parsed(variables, "JORB_RUN_DIR")?,
            cloud_hypervisor: parsed(variables, "JORB_CH_BIN")?,
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

    /// One name for the host, because one request runs at a time.
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

fn parsed<T>(variables: &HashMap<String, String>, name: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let raw = required(variables, name)?;
    raw.parse().with_context(|| format!("{name} is {raw:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_is_required_and_checked() {
        assert!(Settings::parse(&crate::test_variables(&[])).is_err());

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
                Settings::parse(&crate::test_variables(&variables)).is_err(),
                "{name}={value}"
            );
        }
    }
}
