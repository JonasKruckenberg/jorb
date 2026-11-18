//! One Amp orb job: read the environment, take the lock, converge, report.
//!
//! The story is in that order and so is this file. `run_job` below is the
//! whole of it.
//!
//! jorb is crash-only. The start path is the recovery path, nothing is cleaned
//! up on the way out, and no error branch below tidies anything.

mod constants;
mod host;
mod init;
mod orb;
mod preflight;
mod request;
mod systemd;

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use clap::Parser;
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
#[derive(Debug, Parser)]
#[command(about = "Runs one Amp orb job on this host")]
struct Invocation {
    #[command(flatten)]
    request: Request,

    #[command(flatten)]
    host: HostConfig,
}

/// `jorb preflight`. Everything it needs comes from `JORB_*`, so it takes no
/// arguments of its own.
#[derive(Debug, Parser)]
#[command(
    name = "jorb preflight",
    about = "Reports whether this host can run orbs"
)]
struct PreflightCommand {
    #[command(flatten)]
    host: HostConfig,
}

/// `jorb init`.
#[derive(Debug, Parser)]
#[command(name = "jorb init", about = "Makes this host ready to run orbs")]
struct InitCommand {
    /// Create and mount an XFS image for the state directory, when the
    /// filesystem it is on cannot reflink. Off by default: it is a
    /// persistent change to the machine.
    #[arg(long)]
    create_state_image: bool,

    /// Where that image goes.
    #[arg(long, default_value = constants::STATE_IMAGE)]
    state_image: std::path::PathBuf,

    /// How big it claims to be. It is sparse, so this is not what it costs.
    #[arg(long, default_value_t = constants::STATE_IMAGE_GIB)]
    state_image_gib: u32,

    #[command(flatten)]
    host: HostConfig,
}

/// Exit 0 means the operation's postcondition was observed. Anything else
/// means it was not, and tells Amp to retry the job.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // The provider calls jorb with no arguments and the job in the
    // environment; a person calls it with a command. Choosing here keeps the
    // job's arguments required for a job and out of a command's way. clap's
    // `subcommand_negates_reqs` does not relax requirements that come from a
    // flattened group, which is all of ours, and the flag that would have to
    // join it treats the `JORB_*` already in a unit's environment as a
    // conflict.
    let arguments: Vec<String> = std::env::args().collect();
    let command = arguments.get(1).cloned();

    // A job explains itself to the dashboards, so it logs everything. A
    // command explains itself to whoever ran it, in the report it prints, so
    // the only events worth having are the ones that went wrong.
    tracing_subscriber::fmt()
        .json()
        .with_max_level(if command.is_some() {
            Level::WARN
        } else {
            Level::DEBUG
        })
        .with_writer(std::io::stderr)
        .init();

    if let Some(command) = command {
        return run_command(&command, &arguments[2..]).await;
    }

    let invocation = match Invocation::try_parse() {
        Ok(invocation) => invocation,
        Err(error) => {
            // clap's own message, as one event instead of on stdout.
            error!(outcome = "rejected", error = error.to_string(), "bad job");
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

/// The commands for whoever installs jorb, as against the provider's job.
///
/// Each parses the rest of the command line itself, so each gets clap's own
/// help and its own refusals.
async fn run_command(command: &str, arguments: &[String]) -> ExitCode {
    let argv = || std::iter::once(format!("jorb {command}")).chain(arguments.iter().cloned());

    let outcome = match command {
        "preflight" => match PreflightCommand::try_parse_from(argv()) {
            Ok(parsed) => preflight::preflight(&parsed.host).await,
            Err(error) => return reject(error),
        },
        "init" => match InitCommand::try_parse_from(argv()) {
            Ok(parsed) => {
                let image = parsed.create_state_image.then_some(init::StateImage {
                    path: parsed.state_image,
                    gib: parsed.state_image_gib,
                });
                init::init(&parsed.host, image.as_ref()).await
            }
            Err(error) => return reject(error),
        },
        _ => {
            error!(
                command,
                "jorb runs one job from the environment, or the commands `preflight` and `init`"
            );
            return ExitCode::FAILURE;
        }
    };

    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            error!(error = format!("{error:#}"), "the command could not finish");
            ExitCode::FAILURE
        }
    }
}

/// clap's own message, as clap formats it.
///
/// A command is run by a person, so its output is for a terminal rather than
/// for the dashboards: `--help` arrives here as an error and is not a failure,
/// and a real mistake reads better as clap's own paragraph than as one field
/// of a JSON event.
fn reject(error: clap::Error) -> ExitCode {
    let failed = error.use_stderr();
    let _ = error.print();

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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

    for directory in [host.disks_directory(), host.run_directory().to_path_buf()] {
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
    }

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
