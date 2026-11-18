//! One Amp orb job: parse the environment, take the lock, recover, run the
//! operation, report. Exit 0 means the operation's postcondition was
//! observed.
//!
//! jorb is crash-only: the start path is the recovery path, nothing is
//! cleaned up on the way out, and no error branch tidies anything.

mod job;
mod limits;
mod operations;
mod settings;
mod unit;
mod vm;

use std::fs::{File, OpenOptions, TryLockError};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use tracing::{Level, debug, error, info, info_span};

use crate::job::{Job, Operation};
use crate::settings::Settings;

/// The process environment in a job, a table in a test.
pub type Environment<'a> = &'a dyn Fn(&str) -> Option<String>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(Level::DEBUG)
        .with_writer(std::io::stderr)
        .init();

    let environment = |name: &str| std::env::var(name).ok();
    let parsed = if std::env::args_os().len() > 1 {
        Err(anyhow::anyhow!(
            "jorb takes no arguments: the job is in the environment"
        ))
    } else {
        Job::from_environment(&environment)
            .and_then(|job| Ok((job, Settings::from_environment(&environment)?)))
    };
    let (job, settings) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            error!(
                outcome = "rejected",
                error = format!("{error:#}"),
                "bad job"
            );
            return ExitCode::FAILURE;
        }
    };

    let _span = info_span!("job", op = job.operation.name(), orb = %job.orb).entered();
    let started = Instant::now();
    let result = run_job(&job, &settings).await;
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(()) => {
            info!(outcome = "ok", elapsed_ms, "job done");
            ExitCode::SUCCESS
        }
        Err(error) => {
            error!(
                outcome = "failed",
                error = format!("{error:#}"),
                elapsed_ms,
                "job failed"
            );
            ExitCode::FAILURE
        }
    }
}

async fn run_job(job: &Job, settings: &Settings) -> Result<()> {
    let _lock = lock(settings).await?;
    timeout(limits::JOB_DEADLINE, async {
        let running = operations::recover(settings).await?;
        match &job.operation {
            Operation::Spawn(config_disk) => {
                operations::spawn(settings, &job.orb, config_disk, &running).await
            }
            Operation::Pause => operations::pause(settings, &job.orb).await,
            Operation::Delete => operations::delete(settings, &job.orb).await,
        }
    })
    .await
    .with_context(|| format!("the job did not finish within {:?}", limits::JOB_DEADLINE))?
}

/// The host's one lock, held for the whole job, so no two jobs interleave.
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
                bail!("another job held the lock for {:?}", limits::LOCK_WAIT)
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

pub fn required(environment: Environment, name: &str) -> Result<String> {
    environment(name).with_context(|| format!("{name} is not set"))
}

/// Runs a program with separate arguments and returns its stdout. No shell,
/// because `ORB_ID` reaches some arguments; no token in the child's
/// environment; and a child that outlives the timeout is killed.
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

#[cfg(test)]
pub fn test_environment(variables: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let variables: Vec<(String, String)> = variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    move |wanted| {
        variables
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.clone())
    }
}
