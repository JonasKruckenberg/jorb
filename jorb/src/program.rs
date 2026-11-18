//! How jorb runs a program: with separate arguments, a timeout, and an
//! environment without the token. `unit.rs` and `vm.rs` both run programs,
//! and this is the one way they do it.

use anyhow::{Context, Result, ensure};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::debug;

use crate::limits;

/// Runs a program and gives back its stdout, failing if it does. Arguments
/// are passed as arguments: nothing jorb runs goes through a shell, because
/// `ORB_ID` reaches some of them. The child never sees the token, and one
/// that outlives the timeout is killed.
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
