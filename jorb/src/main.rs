//! One Amp orb job: read the environment, take the lock, converge, report.
//!
//! An orb's state is two facts, disk exists and VM running, and every
//! operation is a target state built from four primitives. jorb is crash-only.
//! The start path is the recovery path, nothing is cleaned up on the way out,
//! and whatever a killed job left behind belongs to the next job's `recover`.
//! No error branch below tidies anything up.
//!
//! Each primitive checks its own postcondition by looking at the world: the
//! VMM's own answer, a file, an interface. No primitive trusts another's exit
//! code or expects a predecessor to have left things tidy.

mod host;
mod job;
mod limits;

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use jorb_vmm::{VmBuilder, VmState};
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use tracing::{Level, debug, error, info, info_span, instrument};

use crate::host::{Slot, cmdline, orb_of_unit, unit_name, unit_pattern};
use crate::job::{ConfigDisk, Operation, OrbId, Request, Settings};

/// The whole job: parse, lock, observe, converge, report.
///
/// Exit 0 means the operation's postcondition was observed. Anything else
/// means it was not, and tells Amp to retry the job.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(Level::DEBUG)
        .with_writer(std::io::stderr)
        .init();

    let request = match Request::try_parse() {
        Ok(request) => request,
        Err(error) => {
            // clap's own message, as one event instead of on stdout.
            error!(outcome = "rejected", error = error.to_string(), "bad job");
            return ExitCode::FAILURE;
        }
    };

    let span = info_span!("job", op = ?request.operation, orb = %request.orb);
    let _entered = span.enter();

    let started = Instant::now();
    match converge(&request).await {
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

/// Holds the lock for the whole job, then runs recover and the operation
/// inside the deadline.
///
/// The deadline sits below Amp's five minutes: a job still running when Amp
/// gives up would become a second job against the same orb.
async fn converge(request: &Request) -> Result<Transition> {
    let settings = &request.settings;
    let orb = &request.orb;

    for directory in [host::disks_directory(settings), settings.run_dir.clone()] {
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
    }

    let _lock = take_lock(&host::lock(settings)).await?;

    timeout(limits::JOB_DEADLINE, async {
        recover(settings).await?;

        // Recover has just stopped every unit whose VM was not running, so
        // under the lock this is either Running or Absent.
        let before = observe(settings, orb).await?;

        match request.operation {
            Operation::Spawn => spawn(request, before).await?,
            Operation::Pause => stop(settings, orb).await?,
            Operation::Delete => {
                stop(settings, orb).await?;
                remove_disk(settings, orb).await?;
            }
        }

        let after = observe(settings, orb).await?;

        Ok(Transition { before, after })
    })
    .await
    .with_context(|| format!("the job did not finish within {:?}", limits::JOB_DEADLINE))?
}

/// The one lock, held for the whole job, so no two jobs interleave.
///
/// The wait is bounded. Every holder has the same job deadline, so a lock held
/// for longer belongs to a stuck job, and failing here lets Amp retry instead
/// of piling jobs up behind it.
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

/// What the VMM behind this orb's socket says about it.
async fn observe(settings: &Settings, orb: &OrbId) -> Result<VmState> {
    Ok(jorb_vmm::state(&host::api_socket(settings, orb)).await?)
}

/// Stops every orb unit whose VM is not running, whichever orb it belongs to.
///
/// Under the lock no job is mid-start, so such a unit is debris from a job that
/// died, and stopping it frees the slot its half-started VM was holding. Only
/// an orb's unit matches the pattern, so the provider's own unit and everything
/// else on the host are out of reach here.
#[instrument(skip_all)]
async fn recover(settings: &Settings) -> Result<()> {
    let units = orb_units().await?;
    debug!(count = units.len(), "orb units");

    for unit in units {
        let Some(orb) = orb_of_unit(&unit) else {
            info!(unit = %unit, "a unit that is not named after an orb; leaving it alone");
            continue;
        };

        let state = observe(settings, &orb).await?;
        if state == VmState::Running {
            continue;
        }

        info!(orb = %orb, state = ?state, "stopping a unit whose VM is not running");
        stop(settings, &orb).await?;
    }

    Ok(())
}

/// The units systemd knows about for this host's orbs, bounded by the number
/// of slots a host can have.
async fn orb_units() -> Result<Vec<String>> {
    let listed = run(
        limits::SYSTEMCTL,
        &[
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            "--no-pager",
            &unit_pattern(),
        ],
    )
    .await?;

    Ok(listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.ends_with(limits::UNIT_SUFFIX))
        .take(limits::MAX_UNITS_TO_RECOVER)
        .map(str::to_owned)
        .collect())
}

/// Stops an orb's unit and takes its socket away.
///
/// The postcondition is systemd's own answer, not the exit code of the stop. A
/// unit that was already gone and a unit that had to be killed look the same
/// from here, so repeating this is safe.
#[instrument(skip_all, fields(orb = %orb))]
async fn stop(settings: &Settings, orb: &OrbId) -> Result<()> {
    let unit = unit_name(orb);

    // --no-block would leave the postcondition to a race; this waits.
    let _ = run(limits::SYSTEMCTL, &["--no-ask-password", "stop", &unit]).await;

    let deadline = Instant::now() + limits::UNIT_GONE_WAIT;
    while unit_is_active(&unit).await? {
        ensure!(
            Instant::now() < deadline,
            "{unit} was still active {:?} after being stopped",
            limits::UNIT_GONE_WAIT
        );
        sleep(limits::UNIT_GONE_POLL).await;
    }

    remove_if_present(&host::api_socket(settings, orb))?;

    Ok(())
}

/// systemd's view of a unit. `show` also answers for a unit it has never heard
/// of, calling it inactive, so a unit `--collect` has already taken away needs
/// no special case.
async fn unit_is_active(unit: &str) -> Result<bool> {
    let state = run(
        limits::SYSTEMCTL,
        &["show", "--property=ActiveState", "--value", unit],
    )
    .await?;

    Ok(matches!(
        state.trim(),
        "active" | "activating" | "deactivating" | "reloading"
    ))
}

/// Gives the orb a disk if it has none, by cloning the base image.
///
/// The clone is written to a partial name and renamed, so a kill in the middle
/// leaves either no disk or a whole one. The partial name is truncated on
/// entry instead of being trusted to be absent.
#[instrument(skip_all)]
async fn ensure_disk(settings: &Settings, orb: &OrbId) -> Result<()> {
    let disk = host::disk(settings, orb);
    if disk.try_exists()? {
        debug!("the disk is already there");
        return Ok(());
    }

    let image = host::base_image(settings);
    let base = File::open(&image).with_context(|| {
        format!(
            "opening the base image at {}; has jorb-prepare-image run?",
            image.display()
        )
    })?;

    let partial_path = host::partial_disk(settings);
    let partial = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&partial_path)
        .with_context(|| format!("creating {}", partial_path.display()))?;

    // A reflink, so a 64 GiB disk costs what the orb writes to it. XFS is what
    // makes this possible, which is why G3 checks for it.
    rustix::fs::ioctl_ficlone(&partial, &base)
        .with_context(|| format!("reflinking the base image to {}", partial_path.display()))?;
    partial
        .sync_all()
        .with_context(|| format!("flushing {}", partial_path.display()))?;
    drop(partial);

    std::fs::rename(&partial_path, &disk)
        .with_context(|| format!("renaming the partial disk to {}", disk.display()))?;

    // The rename has to survive a power cut too, which means fsyncing the
    // directory that holds the name.
    let directory = host::disks_directory(settings);
    rustix::fs::fsync(&File::open(&directory)?)
        .with_context(|| format!("flushing {}", directory.display()))?;

    ensure!(
        disk.try_exists()?,
        "the disk at {} is not there after being created",
        disk.display()
    );
    info!(disk = %disk.display(), "disk cloned from the base image");

    Ok(())
}

/// Takes the orb's disk and its config disk away. Repeating this is a no-op.
#[instrument(skip_all)]
async fn remove_disk(settings: &Settings, orb: &OrbId) -> Result<()> {
    let disk = host::disk(settings, orb);

    remove_if_present(&disk)?;
    remove_if_present(&host::config_disk(settings, orb))?;

    ensure!(
        !disk.try_exists()?,
        "the disk at {} is still there after being removed",
        disk.display()
    );

    Ok(())
}

/// Spawn: leave a healthy orb alone, otherwise start it from where it is.
///
/// Healthy means a running VM with a byte-identical config disk. A changed
/// token leaves the old VM unable to talk to Amp, so it is stopped and started
/// again with the new one. `state` is the observation converge made under the
/// same lock, so the VMM is not asked twice.
#[instrument(skip_all)]
async fn spawn(request: &Request, state: VmState) -> Result<()> {
    let settings = &request.settings;
    let orb = &request.orb;
    let config_disk = ConfigDisk::new(request.token.as_ref(), request.amp_url.as_deref())?;

    if state == VmState::Running
        && config_disk_says(&host::config_disk(settings, orb), &config_disk)?
    {
        info!("already running with this config; leaving it alone");
        return Ok(());
    }

    stop(settings, orb).await?;
    ensure_disk(settings, orb).await?;
    start(request, &config_disk).await
}

/// Whether the config disk on this host already says exactly this.
///
/// Byte equality, which is what the config disk's fixed size is for. The
/// contents are never logged: one of the values is the token.
fn config_disk_says(path: &Path, wanted: &ConfigDisk) -> Result<bool> {
    match std::fs::read(path) {
        Ok(existing) => Ok(existing == wanted.as_bytes()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Starts the orb: config disk, slot, unit, VM.
///
/// Nothing is undone if any of this fails. The unit, the socket and the tap
/// left behind are what the next job's `recover` is for; tidying up here would
/// be a second path that a kill skips anyway.
#[instrument(skip_all, fields(orb = %request.orb, slot = tracing::field::Empty))]
async fn start(request: &Request, config_disk: &ConfigDisk) -> Result<()> {
    let settings = &request.settings;
    let orb = &request.orb;
    let socket = host::api_socket(settings, orb);
    let config_disk_path = host::config_disk(settings, orb);

    // Preconditions this primitive establishes for itself.
    remove_if_present(&socket)?;
    write_config_disk(&config_disk_path, config_disk)?;

    let slot = lowest_free_slot(settings)?;
    let unit = unit_name(orb);
    tracing::Span::current().record("slot", slot.index());
    info!(slot = slot.index(), unit = %unit, "starting");

    run(
        limits::SYSTEMD_RUN,
        &[
            "--no-ask-password",
            "--collect",
            &format!("--unit={unit}"),
            &format!("--slice={}", limits::SLICE),
            "--property=Type=exec",
            "--property=OOMScoreAdjust=500",
            &format!(
                "--property=TimeoutStopSec={}",
                limits::UNIT_STOP_TIMEOUT.as_secs()
            ),
            "--",
            &settings.cloud_hypervisor.to_string_lossy(),
            &format!("--api-socket={}", socket.display()),
        ],
    )
    .await
    .with_context(|| format!("starting {unit}"))?;

    wait_for_socket(&socket).await?;

    VmBuilder::new(
        &settings.kernel,
        &cmdline(slot, orb),
        settings.vcpus,
        settings.memory_mib,
    )
    .without_huge_pages()
    .disk(&host::disk(settings, orb))
    .read_only_disk(&config_disk_path)
    .tap(&slot.tap_name(), slot.host_address(), limits::GUEST_NETMASK)
    .free_page_reporting()
    .boot(&socket)
    .await
    .context("creating and booting the VM")?;

    let state = observe(settings, orb).await?;
    ensure!(
        state == VmState::Running,
        "the VM is {state:?} rather than running after being booted"
    );
    info!(slot = slot.index(), "running");

    Ok(())
}

/// The config disk, written where only root can read it. The run directory is
/// a tmpfs, so the token never reaches a disk that survives a reboot.
fn write_config_disk(path: &Path, config_disk: &ConfigDisk) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;

    file.write_all(config_disk.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flushing {}", path.display()))?;

    Ok(())
}

/// The lowest slot whose tap is absent.
///
/// The tap is the only record of a slot being taken: Cloud Hypervisor creates
/// it on boot and it goes away with the process, so there is no counter to get
/// out of step with reality. No free slot is the host's capacity error.
fn lowest_free_slot(settings: &Settings) -> Result<Slot> {
    let slots = usize::from(settings.slots.get()).min(limits::MAX_SLOTS);

    (0..=u8::MAX)
        .take(slots)
        .map(Slot::new)
        .find(|slot| {
            !Path::new(limits::SYS_CLASS_NET)
                .join(slot.tap_name())
                .exists()
        })
        .with_context(|| format!("no free slot: all {slots} are taken"))
}

/// Waits for Cloud Hypervisor to open its API socket.
async fn wait_for_socket(socket: &Path) -> Result<()> {
    let deadline = Instant::now() + limits::API_SOCKET_WAIT;

    while !socket.exists() {
        ensure!(
            Instant::now() < deadline,
            "{} did not appear within {:?}",
            socket.display(),
            limits::API_SOCKET_WAIT
        );
        sleep(limits::API_SOCKET_POLL).await;
    }

    Ok(())
}

/// Removes a path if it is there. A path that was already gone is the same
/// outcome, which is what makes every primitive safe to repeat.
fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

/// Runs a program and gives back its stdout, failing if it does.
///
/// Arguments are passed as arguments: nothing jorb runs goes through a shell,
/// because `ORB_ID` reaches some of them. Every caller wants a program that
/// succeeded, so there is one way to run one.
async fn run(program: &str, arguments: &[&str]) -> Result<String> {
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

    /// A directory of this test's own, so two tests cannot tread on each
    /// other's files.
    fn test_directory(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join("jorb-tests").join(name);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn a_config_disk_matches_only_its_own_bytes() {
        let path = test_directory("config-disk").join("orb.cfg");
        let wanted = ConfigDisk::new(Some(&"a-token".into()), None).unwrap();
        let other = ConfigDisk::new(Some(&"another-token".into()), None).unwrap();

        std::fs::remove_file(&path).ok();
        assert!(
            !config_disk_says(&path, &wanted).unwrap(),
            "a config disk that is not there cannot match"
        );

        write_config_disk(&path, &wanted).unwrap();

        assert!(config_disk_says(&path, &wanted).unwrap());
        assert!(!config_disk_says(&path, &other).unwrap());
    }

    #[test]
    fn removing_a_path_that_is_not_there_is_not_a_failure() {
        let path = test_directory("removal").join("not-here");

        assert!(remove_if_present(&path).is_ok());
    }
}
