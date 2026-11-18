//! systemd: starting and stopping an orb's unit, and finding the orbs whose
//! units exist.
//!
//! Units are `jorb-orb-<id>.service`. The prefix is what makes clearing dead
//! orbs safe: no other unit on the host can be read as an orb's.

use anyhow::{Context, Result, ensure};

use crate::request::OrbId;
use crate::{command, limits};

const PREFIX: &str = "jorb-orb-";
const SUFFIX: &str = ".service";
/// The host kills an orb before anything else.
const OOM_SCORE_ADJUST: i32 = 500;

/// Starts `program` (with its arguments) as the orb's unit in `jorb.slice`.
/// `--collect` so a failed unit leaves nothing behind; `Type=exec` so systemd
/// waits for the exec. Returns the invocation id, which finds the unit's
/// journal after `--collect` has removed the unit.
pub async fn start(orb: &OrbId, program: &[String]) -> Result<String> {
    let unit = unit(orb);
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
    arguments.extend_from_slice(program);
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    command::run("systemd-run", &arguments)
        .await
        .with_context(|| format!("starting {unit}"))?;
    let invocation = command::run(
        "systemctl",
        &["show", "--property=InvocationID", "--value", &unit],
    )
    .await?;
    Ok(invocation.trim().to_owned())
}

/// Stops the orb's unit. The postcondition is systemd's answer afterwards, not
/// the stop's exit code: an unknown unit fails to stop and is as stopped as
/// can be.
pub async fn stop(orb: &OrbId) -> Result<()> {
    let unit = unit(orb);
    let stopped = command::run("systemctl", &["stop", &unit]).await;
    let state = command::run(
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

/// Every orb with a unit, active or not. Ids go back through the same parser
/// as Amp's.
pub async fn orbs() -> Result<Vec<OrbId>> {
    let pattern = format!("{PREFIX}*{SUFFIX}");
    let listed = command::run(
        "systemctl",
        &["list-units", "--all", "--plain", "--no-legend", &pattern],
    )
    .await?;
    Ok(listed
        .lines()
        .filter_map(|line| orb_of(line.split_whitespace().next()?))
        .take(limits::MAX_UNITS)
        .collect())
}

fn unit(orb: &OrbId) -> String {
    format!("{PREFIX}{orb}{SUFFIX}")
}

fn orb_of(unit: &str) -> Option<OrbId> {
    unit.strip_prefix(PREFIX)?
        .strip_suffix(SUFFIX)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_orbs_unit_names_an_orb() {
        let id: OrbId = "amp-T-1a2b".parse().unwrap();
        assert_eq!(unit(&id), "jorb-orb-amp-T-1a2b.service");
        assert_eq!(orb_of(&unit(&id)), Some(id));

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
