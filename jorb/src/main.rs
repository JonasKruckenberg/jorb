//! One Amp orb job: read the environment, take the lock, converge, report.
//!
//! The story is in that order and so is this file. `run_job` below is the
//! whole of it.
//!
//! jorb is crash-only. The start path is the recovery path, nothing is cleaned
//! up on the way out, and no error branch below tidies anything.

mod constants;
mod host;
mod orb;
mod request;
mod systemd;

use std::env::VarError;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use jorb_vmm::VmState;
use tokio::time::{sleep, timeout};
use tracing::{Level, debug, error, info, info_span, instrument};

use crate::host::HostConfig;
use crate::orb::Orb;
use crate::request::{ConfigDisk, Operation, Request};

/// What one run of jorb was given.
///
/// The two halves come from different places and are trusted differently:
/// untrusted `ORB_*` from the provider, and `JORB_*` from the root-side
/// wrapper. Keeping them apart here puts that boundary in every signature
/// below.
#[derive(Debug)]
struct Invocation {
    request: Request,
    host: HostConfig,
}

/// Where a parser reads its variables: the process environment in a job, a
/// table in a test. A variable that is set but not UTF-8 is an error rather
/// than absent.
pub type Environment<'a> = &'a dyn Fn(&str) -> Result<Option<String>>;

/// One variable from the process environment.
fn process_environment(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => bail!("{name} is set but is not UTF-8"),
    }
}

/// A variable the job cannot do without. Never used for a secret: the name is
/// in the message, and so would be nothing else.
pub fn required(environment: Environment, name: &str) -> Result<String> {
    environment(name)?.with_context(|| format!("{name} is not set"))
}

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

    let parsed = Request::from_environment(&process_environment).and_then(|request| {
        Ok(Invocation {
            request,
            host: HostConfig::from_environment(&process_environment)?,
        })
    });
    let invocation = match parsed {
        Ok(invocation) => invocation,
        Err(error) => {
            error!(
                outcome = "rejected",
                error = format!("{error:#}"),
                "bad job"
            );
            return ExitCode::FAILURE;
        }
    };

    let span = info_span!(
        "job",
        op = ?invocation.request.operation,
        orb = %invocation.request.orb
    );
    let _entered = span.enter();

    let started = Instant::now();
    match run_job(&invocation).await {
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

/// What the job found and what it left behind, for the closing event. Two
/// values of one type, named so they cannot be swapped.
#[derive(Debug)]
struct Transition {
    before: VmState,
    after: VmState,
}

/// One job, start to finish: take the host's lock, clear what a dead job left
/// behind, drive this orb to the operation's target state, and report what
/// changed.
///
/// The deadline sits below Amp's five minutes: a job still running when Amp
/// gives up would become a second job against the same orb.
async fn run_job(invocation: &Invocation) -> Result<Transition> {
    let host = &invocation.host;
    let request = &invocation.request;
    let orb = Orb::new(host, &request.orb);

    let _lock = take_host_lock(&host.lock()).await?;

    timeout(constants::JOB_DEADLINE, async {
        stop_dead_orbs(host).await?;

        // Recover has just stopped every unit whose VM was not running, so
        // under the lock this is either Running or Absent.
        let before = orb.state().await?;

        match request.operation {
            Operation::Spawn => spawn(&orb, request, before).await?,
            Operation::Pause => orb.stop().await?,
            Operation::Delete => {
                orb.stop().await?;
                orb.remove_disk().await?;
            }
        }

        let after = orb.state().await?;

        Ok(Transition { before, after })
    })
    .await
    .with_context(|| {
        format!(
            "the job did not finish within {:?}",
            constants::JOB_DEADLINE
        )
    })?
}

/// The host's one lock, held for the whole job, so no two jobs on this host
/// interleave.
///
/// The wait is bounded. Every holder has the same job deadline, so a lock held
/// for longer belongs to a stuck job, and failing here lets Amp retry instead
/// of piling jobs up behind it.
async fn take_host_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;

    let deadline = Instant::now() + constants::LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                ensure!(
                    Instant::now() < deadline,
                    "another job held {} for longer than {:?}",
                    path.display(),
                    constants::LOCK_WAIT
                );
                debug!("waiting for the lock");
                sleep(constants::LOCK_POLL).await;
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("locking {}", path.display()));
            }
        }
    }
}

/// Stops every orb unit whose VM is not running, whichever orb it belongs to.
/// This is `recover` in AGENTS.md's model, and every job begins with it.
///
/// Under the lock no job is mid-start, so such a unit is debris from a job that
/// died, and stopping it frees the slot its half-started VM was holding.
#[instrument(skip_all)]
async fn stop_dead_orbs(host: &HostConfig) -> Result<()> {
    let units = systemd::orb_units().await?;
    debug!(count = units.len(), "orb units");

    for unit in units {
        let Some(id) = systemd::orb_of_unit(&unit) else {
            info!(unit = %unit, "a unit that is not named after an orb; leaving it alone");
            continue;
        };

        let orb = Orb::new(host, &id);
        let state = orb.state().await?;
        if state == VmState::Running {
            continue;
        }

        info!(orb = %id, state = ?state, "stopping a unit whose VM is not running");
        orb.stop().await?;
    }

    Ok(())
}

/// Spawn: leave a healthy orb alone, otherwise start it from where it is.
///
/// Healthy means a running VM with a byte-identical config disk. A changed
/// token leaves the old VM unable to talk to Amp, so it is stopped and started
/// again with the new one. `state` is the observation converge made under the
/// same lock, so the VMM is not asked twice.
#[instrument(skip_all)]
async fn spawn(orb: &Orb<'_>, request: &Request, state: VmState) -> Result<()> {
    let config_disk = ConfigDisk::new(request.token.as_ref(), request.amp_url.as_deref())?;

    if state == VmState::Running && orb.config_disk_matches(&config_disk)? {
        info!("already running with this config; leaving it alone");
        return Ok(());
    }

    orb.stop().await?;
    orb.ensure_disk().await?;
    orb.start(&config_disk).await
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
