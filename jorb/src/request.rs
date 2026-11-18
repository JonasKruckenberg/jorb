//! A request from Amp, and what applying it does to the orb.
//!
//! The request is parsed once from the untrusted `ORB_*` variables; this is
//! the privilege boundary, and every refusal jorb makes of Amp's input is
//! here. An orb is two facts, *disk exists* and *VM running*, and applying a
//! request first clears the dead orbs, then does its operation:
//!
//! | Operation | Steps |
//! |---|---|
//! | pause | `vm::power_off` |
//! | delete | `vm::power_off`, remove the disk |
//! | spawn | nothing if running with an equal config disk; otherwise `vm::power_off`, `ensure_disk`, `vm::boot` |

use std::fmt;
use std::fs::File;
use std::io;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use jorb_config_disk::ConfigDisk;
use tracing::{info, instrument, warn};

use crate::settings::Settings;
use crate::variables::Variables;
use crate::{limits, systemd, vm};

#[derive(Debug)]
pub struct Request {
    pub orb: OrbId,
    pub kind: RequestKind,
}

#[derive(Debug)]
pub enum RequestKind {
    Spawn(ConfigDisk),
    Pause,
    Delete,
}

impl Request {
    pub fn parse(variables: &Variables) -> Result<Self> {
        let orb = variables.required("ORB_ID")?.parse()?;
        let operation = match variables.required("ORB_OPERATION")? {
            "spawn" => {
                let config_disk = ConfigDisk {
                    orb_token: variables.required("ORB_TOKEN")?.to_owned(),
                    amp_url: variables.optional("AMP_URL").map(str::to_owned),
                };
                ensure!(!config_disk.orb_token.is_empty(), "ORB_TOKEN is empty");
                config_disk.write_to(io::sink())?;
                RequestKind::Spawn(config_disk)
            }
            "pause" => RequestKind::Pause,
            "delete" => RequestKind::Delete,
            other => bail!("ORB_OPERATION is {other:?}, which is not spawn, pause or delete"),
        };
        Ok(Self {
            orb,
            kind: operation,
        })
    }

    /// Drives the request's orb to the state its operation asks for.
    pub async fn apply(&self, settings: &Settings) -> Result<()> {
        let running = clear_dead_orbs(settings).await?;
        let orb = &self.orb;
        match &self.kind {
            RequestKind::Spawn(config_disk) => {
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
            RequestKind::Pause => vm::power_off(settings, orb).await,
            RequestKind::Delete => {
                vm::power_off(settings, orb).await?;
                vm::remove_if_present(&settings.disk_path(orb))
            }
        }
    }
}

impl RequestKind {
    /// As Amp spells it, for the `op` log field.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Spawn(_) => "spawn",
            Self::Pause => "pause",
            Self::Delete => "delete",
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

/// Safe unescaped as a file name, a unit name and a hostname.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbId(String);

impl FromStr for OrbId {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        ensure!(
            raw.len() <= limits::MAX_ORB_ID_LEN
                && raw.starts_with(|first: char| first.is_ascii_alphanumeric())
                && raw
                    .chars()
                    .all(|found| found.is_ascii_alphanumeric() || found == '-' || found == '_'),
            "{raw:?} is not an orb id: up to {} letters, digits, - and _, starting with a letter or digit",
            limits::MAX_ORB_ID_LEN
        );
        Ok(Self(raw.to_owned()))
    }
}

impl fmt::Display for OrbId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(variables: &[(&str, &str)]) -> Result<Request> {
        Request::parse(&Variables::of(variables))
    }

    #[test]
    fn ids_are_safe_as_paths_units_and_hostnames() {
        let longest = "a".repeat(limits::MAX_ORB_ID_LEN);
        for valid in ["amp-T-1a2b3c4d", "a", "orb_1", &longest] {
            assert_eq!(valid.parse::<OrbId>().unwrap().to_string(), valid);
        }

        let too_long = "a".repeat(limits::MAX_ORB_ID_LEN + 1);
        for invalid in [
            "", &too_long, "-rf", "_x", "../x", ".", "/", "a.b", "a/b", "a b", "a\nb", "a\0b",
            "a$b",
        ] {
            assert!(invalid.parse::<OrbId>().is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn only_a_spawn_needs_a_token_and_it_must_not_be_empty() {
        let pause = parse(&[("ORB_OPERATION", "pause"), ("ORB_ID", "amp-1")]).unwrap();
        assert_eq!(pause.kind.name(), "pause");

        for token in [None, Some("")] {
            let mut variables = vec![("ORB_OPERATION", "spawn"), ("ORB_ID", "amp-1")];
            variables.extend(token.map(|token| ("ORB_TOKEN", token)));
            let refusal = format!("{:#}", parse(&variables).unwrap_err());
            assert!(refusal.contains("ORB_TOKEN"), "{refusal}");
        }

        for operation in ["destroy", "Spawn"] {
            assert!(parse(&[("ORB_OPERATION", operation), ("ORB_ID", "amp-1")]).is_err());
        }
    }

    #[test]
    fn the_token_reaches_the_config_disk_and_nowhere_else() {
        let request = parse(&[
            ("ORB_OPERATION", "spawn"),
            ("ORB_ID", "amp-1"),
            ("ORB_TOKEN", "must-not-leak"),
            ("AMP_URL", "https://amp.example"),
        ])
        .unwrap();
        assert!(!format!("{request:?}").contains("must-not-leak"));

        let RequestKind::Spawn(disk) = request.kind else {
            panic!("a spawn");
        };
        assert_eq!(disk.orb_token, "must-not-leak");
        assert_eq!(disk.amp_url.as_deref(), Some("https://amp.example"));
    }
}
