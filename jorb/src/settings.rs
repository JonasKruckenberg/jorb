//! What this host is configured as: how many orbs it runs, how big each is,
//! and where its files and programs are.
//!
//! Every value comes from a `JORB_*` variable the root-side wrapper sets, so
//! none of it is the caller's to choose. See Security in `AGENTS.md`.

use std::num::{NonZeroU8, NonZeroU32};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};

use crate::{Environment, limits, required};

/// Where the base image and the orbs' disks live, unless `JORB_STATE_DIR` says.
const STATE_DIRECTORY: &str = "/var/lib/jorb";

/// Where the config disks and API sockets live, unless `JORB_RUN_DIR` says.
const RUN_DIRECTORY: &str = "/run/jorb";

/// The VMM, by name, unless `JORB_CH_BIN` gives a path.
const CLOUD_HYPERVISOR: &str = "cloud-hypervisor";

#[derive(Debug)]
pub struct Settings {
    /// How many orbs may run at once, `1..=MAX_SLOTS`.
    pub slots: u16,
    pub vcpus: NonZeroU8,
    pub memory_mib: NonZeroU32,
    pub kernel: PathBuf,
    /// XFS, so each orb's disk is a reflink clone.
    pub state_directory: PathBuf,
    /// A tmpfs, so a host reboot clears every config disk.
    pub run_directory: PathBuf,
    /// A setting so the bounds tests can point it at a program that never
    /// opens its socket.
    pub cloud_hypervisor: PathBuf,
}

impl Settings {
    pub fn from_environment(environment: Environment) -> Result<Self> {
        let slots = setting(environment, "JORB_SLOTS", 1)?;
        ensure!(
            (1..=limits::MAX_SLOTS).contains(&slots),
            "JORB_SLOTS is {slots}, and a host has between 1 and {} slots",
            limits::MAX_SLOTS
        );

        Ok(Self {
            slots,
            vcpus: setting(environment, "JORB_VCPUS", limits::DEFAULT_VCPUS)?,
            memory_mib: setting(environment, "JORB_MEMORY_MIB", limits::DEFAULT_MEMORY_MIB)?,
            kernel: required(environment, "JORB_KERNEL")?.into(),
            state_directory: setting(environment, "JORB_STATE_DIR", STATE_DIRECTORY.into())?,
            run_directory: setting(environment, "JORB_RUN_DIR", RUN_DIRECTORY.into())?,
            cloud_hypervisor: setting(environment, "JORB_CH_BIN", CLOUD_HYPERVISOR.into())?,
        })
    }
}

/// A setting, or its default when the variable is not set.
fn setting<T>(environment: Environment, name: &str, default: T) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match environment(name)? {
        Some(raw) => raw.parse().with_context(|| format!("{name} is {raw:?}")),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::test_environment;

    #[test]
    fn the_settings_describe_the_v1_machine_by_default() {
        let settings = Settings::from_environment(&test_environment(&[(
            "JORB_KERNEL",
            "/var/lib/jorb/kernel",
        )]))
        .unwrap();

        assert_eq!(settings.slots, 1);
        assert_eq!(settings.vcpus.get(), 4);
        assert_eq!(settings.memory_mib.get(), 6144);
        assert_eq!(settings.state_directory, Path::new("/var/lib/jorb"));
        assert_eq!(settings.run_directory, Path::new("/run/jorb"));
        assert_eq!(settings.cloud_hypervisor, Path::new("cloud-hypervisor"));
    }

    #[test]
    fn a_setting_outside_what_the_host_can_express_is_refused() {
        for (name, value) in [
            ("JORB_SLOTS", "0"),
            ("JORB_SLOTS", "257"),
            ("JORB_VCPUS", "0"),
            ("JORB_MEMORY_MIB", "lots"),
        ] {
            let environment =
                test_environment(&[("JORB_KERNEL", "/var/lib/jorb/kernel"), (name, value)]);
            assert!(
                Settings::from_environment(&environment).is_err(),
                "{name}={value} should be refused"
            );
        }

        assert!(Settings::from_environment(&test_environment(&[])).is_err());
    }
}
