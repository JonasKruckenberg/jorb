//! One request from Amp: parse the environment, take the lock, run the
//! operation, report. Exit 0 means the operation's postcondition was
//! observed.
//!
//! jorb is crash-only: the start path is the recovery path, nothing is
//! cleaned up on the way out, and no error branch tidies anything.

mod limits;
mod operations;
mod request;
mod settings;
mod systemd;
mod vm;

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use tracing::{Level, debug, error, info, info_span};

use crate::request::Request;
use crate::settings::Settings;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(Level::DEBUG)
        .with_writer(std::io::stderr)
        .init();

    let parsed = environment().and_then(|variables| {
        ensure!(std::env::args_os().len() == 1, "jorb takes no arguments");
        Ok((Request::parse(&variables)?, Settings::parse(&variables)?))
    });
    let (request, settings) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            error!(
                outcome = "rejected",
                error = format!("{error:#}"),
                "bad request"
            );
            return ExitCode::FAILURE;
        }
    };

    let _span = info_span!("request", op = request.operation.name(), orb = %request.orb).entered();
    let started = Instant::now();
    let result = run(&request, &settings).await;
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(()) => {
            info!(outcome = "ok", elapsed_ms, "request done");
            ExitCode::SUCCESS
        }
        Err(error) => {
            error!(
                outcome = "failed",
                error = format!("{error:#}"),
                elapsed_ms,
                "request failed"
            );
            ExitCode::FAILURE
        }
    }
}

/// The variables jorb reads: `ORB_*` and `AMP_URL` from Amp, `JORB_*` from the
/// root-side wrapper. One of those that is not UTF-8 is an error rather than
/// absent; anything else in the environment is not jorb's business.
fn environment() -> Result<HashMap<String, String>> {
    let mut variables = HashMap::new();
    for (name, value) in std::env::vars_os() {
        let Ok(name) = name.into_string() else {
            continue;
        };
        let ours = name.starts_with("ORB_") || name.starts_with("JORB_") || name == "AMP_URL";
        match value.into_string() {
            Ok(value) if ours => {
                variables.insert(name, value);
            }
            Err(_) if ours => bail!("{name} is not UTF-8"),
            _ => {}
        }
    }
    Ok(variables)
}

pub fn required<'a>(variables: &'a HashMap<String, String>, name: &str) -> Result<&'a str> {
    variables
        .get(name)
        .map(String::as_str)
        .with_context(|| format!("{name} is not set"))
}

async fn run(request: &Request, settings: &Settings) -> Result<()> {
    let _lock = lock(settings).await?;
    timeout(limits::REQUEST_DEADLINE, operations::run(settings, request))
        .await
        .with_context(|| format!("not finished within {:?}", limits::REQUEST_DEADLINE))?
}

/// The host's one lock, held for the whole request, so no two interleave.
async fn lock(settings: &Settings) -> Result<File> {
    let path = settings.lock_path();
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;

    let deadline = Instant::now() + limits::LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                sleep(limits::LOCK_POLL).await;
            }
            Err(TryLockError::WouldBlock) => {
                bail!("another request held the lock for {:?}", limits::LOCK_WAIT)
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

/// Runs a program with separate arguments and returns its stdout. No shell,
/// because `ORB_ID` reaches some arguments; no token in the child's
/// environment; and a child that outlives the timeout is killed.
pub async fn run_program(program: &str, arguments: &[&str]) -> Result<String> {
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

#[cfg(test)]
pub fn test_variables(variables: &[(&str, &str)]) -> HashMap<String, String> {
    variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}
