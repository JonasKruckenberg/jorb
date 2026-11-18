//! systemd: an orb's unit, which ones exist, starting and stopping one.
//!
//! The `jorb-orb-` prefix is what makes recover safe: no other unit on the
//! host can be read as an orb's.

use anyhow::{Context, Result, ensure};

use crate::job::OrbId;
use crate::{limits, run};

const PREFIX: &str = "jorb-orb-";
/// The host kills an orb before anything else.
const OOM_SCORE_ADJUST: i32 = 500;
const SUFFIX: &str = ".service";

pub fn name(orb: &OrbId) -> String {
    format!("{PREFIX}{orb}{SUFFIX}")
}

/// The orb a unit belongs to. The id goes back through the same parser as
/// Amp's.
pub fn orb_of(unit: &str) -> Option<OrbId> {
    unit.strip_prefix(PREFIX)?
        .strip_suffix(SUFFIX)?
        .parse()
        .ok()
}

/// Every orb unit systemd knows about, active or not.
pub async fn orb_units() -> Result<Vec<String>> {
    let pattern = format!("{PREFIX}*{SUFFIX}");
    let listed = run(
        "systemctl",
        &["list-units", "--all", "--plain", "--no-legend", &pattern],
    )
    .await?;
    Ok(listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .take(limits::MAX_UNITS)
        .map(str::to_owned)
        .collect())
}

/// `--collect` so a failed unit leaves nothing behind; `Type=exec` so systemd
/// waits for the exec. Returns the invocation id, which finds the unit's
/// journal after `--collect` has removed the unit.
pub async fn start(orb: &OrbId, command: &[String]) -> Result<String> {
    let unit = name(orb);
    let mut arguments = vec![
        "--collect".to_owned(),
        format!("--unit={unit}"),
        "--slice=jorb.slice".to_owned(),
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

    run("systemd-run", &arguments)
        .await
        .with_context(|| format!("starting {unit}"))?;
    let invocation = run(
        "systemctl",
        &["show", "--property=InvocationID", "--value", &unit],
    )
    .await?;
    Ok(invocation.trim().to_owned())
}

/// `systemctl stop` waits for the stop to finish, and its exit code is not the
/// postcondition: an unknown unit fails to stop and is as stopped as can be.
/// systemd's answer afterwards is.
pub async fn stop(orb: &OrbId) -> Result<()> {
    let unit = name(orb);
    let stopped = run("systemctl", &["stop", &unit]).await;
    let state = run(
        "systemctl",
        &["show", "--property=ActiveState", "--value", &unit],
    )
    .await?;
    ensure!(
        matches!(state.trim(), "inactive" | "failed"),
        "{unit} is {} after being stopped; the stop said: {:?}",
        state.trim(),
        stopped.err()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_orbs_unit_names_an_orb() {
        let id: OrbId = "amp-T-1a2b".parse().unwrap();
        assert_eq!(name(&id), "jorb-orb-amp-T-1a2b.service");
        assert_eq!(orb_of(&name(&id)), Some(id));

        for unit in [
            "jorb-provider.service",
            "jorb-gate-g4.service",
            "jorb-orb-.service",
            "jorb-orb-amp-1.slice",
            "jorb-orb-../x.service",
        ] {
            assert!(orb_of(unit).is_none(), "{unit}");
        }
    }
}
