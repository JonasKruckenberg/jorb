//! systemd: what an orb's unit is called, how to start and stop one, which
//! ones exist, and how jorb runs a program at all.
//!
//! The `jorb-orb-` prefix is the whole reason recover is safe: no other unit
//! on the host, the provider's own included, can be read as an orb's. What
//! the unit runs is `vm.rs`'s business.

use std::time::Instant;

use anyhow::{Context, Result, ensure};
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use tracing::debug;

use crate::job::OrbId;
use crate::limits;

const UNIT_PREFIX: &str = "jorb-orb-";
const UNIT_SUFFIX: &str = ".service";

/// The slice every orb runs in, so one `MemoryHigh` covers all of them.
const SLICE: &str = "jorb.slice";

/// By name, so the PATH the wrapper sets decides which systemd this is.
const SYSTEMCTL: &str = "systemctl";
const SYSTEMD_RUN: &str = "systemd-run";

/// A high OOM score, so the host kills an orb before anything else.
const OOM_SCORE_ADJUST: i32 = 500;

pub fn unit_name(orb: &OrbId) -> String {
    format!("{UNIT_PREFIX}{orb}{UNIT_SUFFIX}")
}

/// The orb a unit belongs to, or `None` when the name is not an orb's. The id
/// goes back through the same parser as the one Amp sent.
pub fn orb_of_unit(unit: &str) -> Option<OrbId> {
    unit.strip_prefix(UNIT_PREFIX)
        .and_then(|rest| rest.strip_suffix(UNIT_SUFFIX))
        .and_then(|id| id.parse().ok())
}

/// The orb units systemd knows about, active or not.
pub async fn orb_units() -> Result<Vec<String>> {
    let pattern = format!("{UNIT_PREFIX}*{UNIT_SUFFIX}");
    let listed = run(
        SYSTEMCTL,
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
        .filter(|unit| unit.ends_with(UNIT_SUFFIX))
        .take(limits::MAX_UNITS_TO_RECOVER)
        .map(str::to_owned)
        .collect())
}

/// Starts `command` as the orb's unit. `--collect` so a failed unit leaves
/// nothing behind, `Type=exec` so systemd waits for the exec.
pub async fn start(orb: &OrbId, command: &[String]) -> Result<String> {
    let unit = unit_name(orb);

    let mut arguments = vec![
        "--no-ask-password".to_owned(),
        "--collect".to_owned(),
        format!("--unit={unit}"),
        format!("--slice={SLICE}"),
        "--property=Type=exec".to_owned(),
        format!("--property=OOMScoreAdjust={OOM_SCORE_ADJUST}"),
        format!(
            "--property=TimeoutStopSec={}",
            limits::UNIT_STOP_TIMEOUT.as_secs()
        ),
        "--".to_owned(),
    ];
    arguments.extend_from_slice(command);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

    run(SYSTEMD_RUN, &arguments)
        .await
        .with_context(|| format!("starting {unit}"))?;

    Ok(unit)
}

/// Stops the orb's unit and waits for systemd to agree it is gone. The
/// postcondition is systemd's answer, not the stop's exit code, so a unit
/// that was already gone and one that had to be killed look the same.
pub async fn stop(orb: &OrbId) -> Result<()> {
    let unit = unit_name(orb);

    // Without --no-block, so this waits for the stop job.
    let _ = run(SYSTEMCTL, &["--no-ask-password", "stop", &unit]).await;

    let deadline = Instant::now() + limits::UNIT_GONE_WAIT;
    while is_active(&unit).await? {
        ensure!(
            Instant::now() < deadline,
            "{unit} was still active {:?} after being stopped",
            limits::UNIT_GONE_WAIT
        );
        sleep(limits::UNIT_GONE_POLL).await;
    }

    Ok(())
}

/// `show` calls a unit it has never heard of inactive, so a unit `--collect`
/// has already removed needs no special case.
async fn is_active(unit: &str) -> Result<bool> {
    let state = run(
        SYSTEMCTL,
        &["show", "--property=ActiveState", "--value", unit],
    )
    .await?;

    Ok(matches!(
        state.trim(),
        "active" | "activating" | "deactivating" | "reloading"
    ))
}

/// Runs a program and gives back its stdout, failing if it does. Arguments
/// are passed as arguments: nothing jorb runs goes through a shell, because
/// `ORB_ID` reaches some of them.
pub async fn run(program: &str, arguments: &[&str]) -> Result<String> {
    debug!(program, ?arguments, "running");

    let output = timeout(
        limits::SUBPROCESS_TIMEOUT,
        Command::new(program).args(arguments).output(),
    )
    .await
    .with_context(|| {
        format!(
            "{program} did not finish within {:?}",
            limits::SUBPROCESS_TIMEOUT
        )
    })?
    .with_context(|| format!("running {program}"))?;

    ensure!(
        output.status.success(),
        "{program} exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_name_and_the_orb_it_names_are_inverses() {
        let id: OrbId = "amp-T-1a2b".parse().unwrap();

        assert_eq!(unit_name(&id), "jorb-orb-amp-T-1a2b.service");
        assert_eq!(orb_of_unit(&unit_name(&id)), Some(id));
    }

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
