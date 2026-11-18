//! One Amp orb job: parse the environment, take the lock, recover, run the
//! operation, report.
//!
//! The story is in that order and so is this file. jorb is crash-only: the
//! start path is the recovery path, nothing is cleaned up on the way out, and
//! no error branch tidies anything.

mod disk;
mod job;
mod limits;
mod operations;
mod settings;
mod unit;
mod vm;

use std::env::VarError;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use jorb_vmm::VmState;
use tokio::time::{sleep, timeout};
use tracing::{Level, debug, error, info, info_span};

use crate::job::Job;
use crate::settings::Settings;

/// Where a parser reads its variables: the process environment in a job, a
/// table in a test. A variable that is set but not UTF-8 is an error rather
/// than absent.
pub type Environment<'a> = &'a dyn Fn(&str) -> Result<Option<String>>;

/// Exit 0 means the operation's postcondition was observed. Anything else
/// means it was not, and tells Amp to retry the job.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(Level::DEBUG)
        .with_writer(std::io::stderr)
        .init();

    // Amp passes the job in the environment and nothing on the command line,
    // so an argument means something other than Amp is calling.
    if let Some(argument) = std::env::args_os().nth(1) {
        error!(
            outcome = "rejected",
            argument = %argument.to_string_lossy(),
            "jorb takes no arguments: the job is in the environment"
        );
        return ExitCode::FAILURE;
    }

    // ORB_* is untrusted and comes from the provider; JORB_* comes from the
    // root-side wrapper. The two are parsed apart and stay apart.
    let parsed = Job::from_environment(&process_environment)
        .and_then(|job| Ok((job, Settings::from_environment(&process_environment)?)));
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

    let span = info_span!("job", op = job.operation(), orb = %job.orb());
    let _entered = span.enter();

    let started = Instant::now();
    match run(&job, &settings).await {
        Ok(states) => {
            info!(
                outcome = "ok",
                state_before = ?states.before,
                state_after = ?states.after,
                changed = states.before != states.after,
                elapsed_ms = started.elapsed().as_millis(),
                "job done"
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            error!(
                outcome = "failed",
                error = format!("{error:#}"),
                elapsed_ms = started.elapsed().as_millis(),
                "job failed"
            );
            ExitCode::FAILURE
        }
    }
}

/// What the job found and what it left behind, for the closing event.
#[derive(Debug)]
struct Transition {
    before: VmState,
    after: VmState,
}

/// Takes the host's lock, recovers, and drives the orb to the operation's
/// target state, all under a deadline below Amp's.
async fn run(job: &Job, settings: &Settings) -> Result<Transition> {
    let orb = job.orb();
    let _lock = take_lock(&disk::lock_path(settings)).await?;

    timeout(limits::JOB_DEADLINE, async {
        // Recover stopped every unit whose VM it could not confirm running,
        // so under the lock an orb is either in this list or absent.
        let running = operations::recover(settings).await?;
        let before = if running.contains(orb) {
            VmState::Running
        } else {
            VmState::Absent
        };

        // Pause and delete stop first and observe afterwards, when the socket
        // is gone, so a VMM that does not answer cannot fail them.
        match job {
            Job::Spawn { config_disk, .. } => {
                operations::spawn(settings, orb, config_disk, &running).await?;
            }
            Job::Pause { .. } => operations::pause(settings, orb).await?,
            Job::Delete { .. } => operations::delete(settings, orb).await?,
        }

        let after = vm::state(settings, orb).await?;
        Ok(Transition { before, after })
    })
    .await
    .with_context(|| format!("the job did not finish within {:?}", limits::JOB_DEADLINE))?
}

/// The host's one lock, held for the whole job, so no two jobs interleave.
/// The wait is bounded so a job behind a stuck one fails and Amp retries.
async fn take_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;

    let deadline = Instant::now() + limits::LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                ensure!(
                    Instant::now() < deadline,
                    "another job held {} for longer than {:?}",
                    path.display(),
                    limits::LOCK_WAIT
                );
                debug!("waiting for the lock");
                sleep(limits::LOCK_POLL).await;
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("locking {}", path.display()));
            }
        }
    }
}

fn process_environment(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => bail!("{name} is set but is not UTF-8"),
    }
}

/// A variable the job cannot do without. Never used for a secret: the name is
/// in the message.
pub fn required(environment: Environment, name: &str) -> Result<String> {
    environment(name)?.with_context(|| format!("{name} is not set"))
}

/// An environment holding exactly these variables, for tests.
#[cfg(test)]
pub fn test_environment(
    variables: &[(&str, &str)],
) -> impl Fn(&str) -> Result<Option<String>> + use<> {
    let variables: Vec<(String, String)> = variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();

    move |wanted| {
        Ok(variables
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.clone()))
    }
}
