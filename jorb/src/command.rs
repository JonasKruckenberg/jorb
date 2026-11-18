//! How jorb runs another program (`systemctl`, `systemd-run`, `nft`): with
//! separate arguments and no shell, because `ORB_ID` reaches some of them;
//! without the token in the child's environment; and under a timeout that
//! kills a child that outlives it.

use anyhow::{Context, Result, ensure};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

use crate::limits;

/// Its stdout, or an error carrying its stderr.
pub async fn run(program: &str, arguments: &[&str]) -> Result<String> {
    debug!(program, ?arguments, "running");
    let output = timeout(
        limits::SUBPROCESS_TIMEOUT,
        Command::new(program)
            .args(arguments)
            .env_remove("ORB_TOKEN")
            .kill_on_drop(true)
            .output(),
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
