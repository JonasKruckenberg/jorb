//! Everything jorb knows about systemd: what an orb's unit is called, how to
//! start and stop one, and how to ask what systemd thinks of it.
//!
//! An orb's lifetime is its unit's, so this is where that lifetime is spoken
//! about. `orb.rs` composes it with the disk and the VMM.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use tokio::time::sleep;

use crate::constants;
use crate::host::{self, HostConfig};
use crate::request::OrbId;

/// The unit an orb runs as.
pub fn unit_name(orb: &OrbId) -> String {
    format!("{}{orb}{}", constants::UNIT_PREFIX, constants::UNIT_SUFFIX)
}

/// The orb a unit belongs to, or `None` when the name is not one of ours.
///
/// The inverse of [`unit_name`]. The id goes back through the same parser as
/// the one Amp sent, so a unit name cannot smuggle in an id `OrbId` refuses.
pub fn orb_of_unit(unit: &str) -> Option<OrbId> {
    unit.strip_prefix(constants::UNIT_PREFIX)
        .and_then(|rest| rest.strip_suffix(constants::UNIT_SUFFIX))
        .and_then(|id| id.parse().ok())
}

/// The orb units systemd knows about on this host, bounded by the number of
/// slots a host can have.
///
/// Only an orb's unit matches the pattern, so the provider's own unit and the
/// gates' `jorb-gate-*` units are out of reach of anything built on this.
pub async fn orb_units() -> Result<Vec<String>> {
    let pattern = format!("{}*{}", constants::UNIT_PREFIX, constants::UNIT_SUFFIX);
    let listed = host::run(
        constants::SYSTEMCTL,
        &[
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            "--no-pager",
            &pattern,
        ],
    )
    .await?;

    Ok(listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.ends_with(constants::UNIT_SUFFIX))
        .take(constants::MAX_UNITS_TO_RECOVER)
        .map(str::to_owned)
        .collect())
}

/// Whether systemd still has this unit running. `show` answers for a unit it
/// has never heard of as well, calling it inactive, so a unit `--collect` has
/// already taken away needs no special case.
pub async fn is_active(unit: &str) -> Result<bool> {
    let state = host::run(
        constants::SYSTEMCTL,
        &["show", "--property=ActiveState", "--value", unit],
    )
    .await?;

    Ok(matches!(
        state.trim(),
        "active" | "activating" | "deactivating" | "reloading"
    ))
}

/// Starts a VMM as an orb's unit, in the slice one `MemoryHigh` covers.
///
/// `--collect` so a failed unit leaves nothing to clean up, `Type=exec` so
/// systemd waits for the VMM to be exec'd, and a high OOM score so the host
/// kills an orb before anything else.
pub async fn start_orb_unit(host: &HostConfig, orb: &OrbId, socket: &Path) -> Result<String> {
    let unit = unit_name(orb);

    host::run(
        constants::SYSTEMD_RUN,
        &[
            "--no-ask-password",
            "--collect",
            &format!("--unit={unit}"),
            &format!("--slice={}", constants::SLICE),
            "--property=Type=exec",
            "--property=OOMScoreAdjust=500",
            &format!(
                "--property=TimeoutStopSec={}",
                constants::UNIT_STOP_TIMEOUT.as_secs()
            ),
            "--",
            &host.cloud_hypervisor.to_string_lossy(),
            &format!("--api-socket={}", socket.display()),
        ],
    )
    .await
    .with_context(|| format!("starting {unit}"))?;

    Ok(unit)
}

/// Stops an orb's unit and waits for systemd to agree it is gone.
///
/// The postcondition is systemd's own answer, not the exit code of the stop.
/// A unit that was already gone and a unit that had to be killed look the
/// same from here, so repeating this is safe.
pub async fn stop_orb_unit(orb: &OrbId) -> Result<()> {
    let unit = unit_name(orb);

    // --no-block would leave the postcondition to a race; this waits.
    let _ = host::run(constants::SYSTEMCTL, &["--no-ask-password", "stop", &unit]).await;

    let deadline = Instant::now() + constants::UNIT_GONE_WAIT;
    while is_active(&unit).await? {
        ensure!(
            Instant::now() < deadline,
            "{unit} was still active {:?} after being stopped",
            constants::UNIT_GONE_WAIT
        );
        sleep(constants::UNIT_GONE_POLL).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orb(raw: &str) -> OrbId {
        raw.parse().expect("the test's id is valid")
    }

    #[test]
    fn a_unit_name_and_the_orb_it_names_are_inverses() {
        let id = orb("amp-T-1a2b");

        assert_eq!(unit_name(&id), "jorb-orb-amp-T-1a2b.service");
        assert_eq!(orb_of_unit(&unit_name(&id)), Some(id));
    }

    /// The `jorb-orb-` prefix keeps the provider's own unit, the gates' units
    /// and everything else on the host out of recover's reach.
    #[test]
    fn a_unit_that_is_not_ours_belongs_to_no_orb() {
        for unit in [
            "sshd.service",
            "jorb-provider.service",
            "jorb-gate-g4.service",
            "jorb-amp-1.service",
            "jorb-orb-.service",
            "jorb-orb-amp-1.slice",
            "jorb-orb-../x.service",
            "jorb-orb-a b.service",
        ] {
            assert!(
                orb_of_unit(unit).is_none(),
                "{unit} should not parse as an orb's unit"
            );
        }
    }
}
