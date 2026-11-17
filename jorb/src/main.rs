//! One Amp orb job: read the environment, take the lock, converge, report.
//!
//! An orb's state is two facts, disk exists and VM running, and every
//! operation is a target state built from four primitives. jorb is crash-only:
//! the start path is the recovery path, nothing is cleaned up on the way out,
//! and whatever a killed job left behind is the next job's `recover` to deal
//! with. That is why nothing below has an error branch that tidies up.
//!
//! Each primitive checks its own postcondition by looking at the world: the
//! VMM's own answer, a file, an interface. No primitive trusts another's exit
//! code, and none of them trusts a predecessor to have left things tidy.

mod job;
mod limits;
mod orb;

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use jorb_vmm::{Vm, VmBuilder, VmState};
use tokio::process::Command;
use tokio::time::{sleep, timeout};
use tracing::{Level, debug, error, info, info_span, instrument};

use crate::job::{Job, Operation, Settings};
use crate::orb::{ConfigDisk, Paths, Slot, cmdline, unit_name};

/// The whole job: parse, lock, observe, converge, report.
///
/// Exit 0 means the operation's postcondition was observed. Anything else
/// means it was not, which is Amp's signal to retry the job.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(Level::DEBUG)
        .with_writer(std::io::stderr)
        .init();

    let job = match Job::try_parse() {
        Ok(job) => job,
        Err(error) => {
            // clap's own message, as one event rather than on stdout.
            error!(outcome = "rejected", error = error.to_string(), "bad job");
            return ExitCode::FAILURE;
        }
    };

    let span = info_span!("job", op = ?job.operation, orb = %job.orb);
    let _entered = span.enter();

    let started = Instant::now();
    match converge(&job).await {
        Ok(outcome) => {
            info!(
                outcome = "ok",
                state_before = ?outcome.before,
                state_after = ?outcome.after,
                changed = outcome.changed,
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
struct Outcome {
    before: VmState,
    after: VmState,
    changed: bool,
}

/// Holds the lock for the whole job, then runs recover and the operation
/// inside the deadline.
///
/// The deadline is below Amp's five minutes on purpose: a job that is still
/// running when Amp gives up would be a second job against the same orb.
async fn converge(job: &Job) -> Result<Outcome> {
    let paths = Paths::new(&job.settings, &job.orb);

    std::fs::create_dir_all(paths.disks_directory())
        .with_context(|| format!("creating {}", paths.disks_directory().display()))?;
    std::fs::create_dir_all(&job.settings.run_dir)
        .with_context(|| format!("creating {}", job.settings.run_dir.display()))?;

    let _lock = take_lock(&paths.lock).await?;

    timeout(limits::JOB_DEADLINE, async {
        recover(&job.settings).await?;

        let before = observe(&paths).await?;
        match job.operation {
            Operation::Spawn => spawn(job, &paths).await?,
            Operation::Pause => stop(&job.orb, &paths).await?,
            Operation::Delete => {
                stop(&job.orb, &paths).await?;
                remove_disk(&paths).await?;
            }
        }
        let after = observe(&paths).await?;

        Ok(Outcome {
            before,
            after,
            changed: before != after,
        })
    })
    .await
    .with_context(|| format!("the job did not finish within {:?}", limits::JOB_DEADLINE))?
}

/// The one lock, held for the whole job, so no two jobs interleave.
///
/// Waiting is bounded: the other holder has the same deadline, so a lock that
/// is still held after that long is a stuck job rather than a busy one, and
/// failing here lets Amp retry.
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

/// What the VMM behind this orb's socket says about it. `Absent` covers both
/// "no socket" and "a socket nothing is listening on".
#[instrument(skip_all)]
async fn observe(paths: &Paths) -> Result<VmState> {
    match Vm::attach(&paths.api_socket).await? {
        Some(vm) => Ok(vm.state().await?),
        None => Ok(VmState::Absent),
    }
}

/// Stops every `jorb-*` unit whose VM is not running, whichever orb it belongs
/// to.
///
/// Under the lock no job is mid-start, so such a unit is debris from a job
/// that died. This is what frees a slot a half-started VM is holding.
#[instrument(skip_all)]
async fn recover(settings: &Settings) -> Result<()> {
    let units = jorb_units().await?;
    debug!(count = units.len(), "units in the slice");

    for unit in units {
        let Some(orb) = orb_of_unit(&unit) else {
            info!(unit = %unit, "a unit that is not named after an orb; leaving it alone");
            continue;
        };

        // The name alone is not enough: jorb-provider.service matches the
        // pattern and would parse as an orb called "provider". An orb's unit
        // is one in jorb.slice, and the provider is not.
        let slice = unit_slice(&unit).await?;
        if slice != limits::SLICE {
            debug!(unit = %unit, slice = %slice, "not in the slice; leaving it alone");
            continue;
        }

        let paths = Paths::new(settings, &orb);
        let state = observe(&paths).await?;

        if state == VmState::Running {
            continue;
        }

        info!(orb = %orb, state = ?state, "stopping a unit whose VM is not running");
        stop(&orb, &paths).await?;
    }

    Ok(())
}

/// The units systemd knows about for this host's orbs, bounded by the number
/// of slots a host can have.
async fn jorb_units() -> Result<Vec<String>> {
    let listed = run(
        limits::SYSTEMCTL,
        &[
            "list-units",
            "--all",
            "--plain",
            "--no-legend",
            "--no-pager",
            limits::UNIT_PATTERN,
        ],
    )
    .await?;

    Ok(listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.ends_with(".service"))
        .take(limits::MAX_UNITS_TO_RECOVER)
        .map(str::to_owned)
        .collect())
}

/// The slice a unit runs in, which is how an orb's unit is told from the
/// provider's.
async fn unit_slice(unit: &str) -> Result<String> {
    let slice = run_allowing_failure(limits::SYSTEMCTL, &["show", "-p", "Slice", "--value", unit])
        .await?
        .trim()
        .to_owned();

    Ok(slice)
}

/// The orb a unit belongs to, or `None` when the name is not one of ours.
/// Reparsing the id puts it through the same rules as the one Amp sent.
fn orb_of_unit(unit: &str) -> Option<job::OrbId> {
    unit.strip_prefix(limits::UNIT_PREFIX)
        .and_then(|rest| rest.strip_suffix(".service"))
        .and_then(|id| id.parse().ok())
}

/// Stops an orb's unit and takes its socket away.
///
/// The postcondition is systemd's own answer, not the exit code of the stop:
/// a unit that was already gone and a unit that had to be killed look the
/// same from here, which is what makes this safe to repeat.
#[instrument(skip_all, fields(orb = %orb))]
async fn stop(orb: &job::OrbId, paths: &Paths) -> Result<()> {
    let unit = unit_name(orb);

    // --no-block would leave the postcondition to a race; this waits.
    let _ = run(limits::SYSTEMCTL, &["--no-ask-password", "stop", &unit]).await;

    let deadline = Instant::now() + limits::UNIT_GONE_WAIT;
    loop {
        if !unit_is_active(&unit).await? {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "{unit} was still active {:?} after being stopped",
            limits::UNIT_GONE_WAIT
        );
        sleep(limits::UNIT_GONE_POLL).await;
    }

    remove_if_present(&paths.api_socket)?;

    Ok(())
}

/// systemd's view of a unit. `is-active` exits non-zero for everything that is
/// not running, which is exactly the question being asked.
async fn unit_is_active(unit: &str) -> Result<bool> {
    let state = run_allowing_failure(limits::SYSTEMCTL, &["is-active", unit]).await?;

    Ok(matches!(
        state.trim(),
        "active" | "activating" | "deactivating" | "reloading"
    ))
}

/// Gives the orb a disk if it has none, by cloning the base image.
///
/// Written to a partial name and renamed, so a kill in the middle leaves
/// either no disk or a whole one. The partial name is truncated on entry
/// rather than trusted to be absent.
#[instrument(skip_all)]
async fn ensure_disk(paths: &Paths) -> Result<()> {
    if paths.disk.try_exists()? {
        debug!("the disk is already there");
        return Ok(());
    }

    let base = File::open(&paths.base_image).with_context(|| {
        format!(
            "opening the base image at {}; has jorb-prepare-image run?",
            paths.base_image.display()
        )
    })?;

    let partial = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&paths.partial_disk)
        .with_context(|| format!("creating {}", paths.partial_disk.display()))?;

    // A reflink, so a 64 GiB disk costs what the orb writes to it. XFS is
    // what makes this possible, which is why G3 checks for it.
    rustix::fs::ioctl_ficlone(&partial, &base).with_context(|| {
        format!(
            "reflinking {} to {}",
            paths.base_image.display(),
            paths.partial_disk.display()
        )
    })?;

    partial
        .sync_all()
        .with_context(|| format!("flushing {}", paths.partial_disk.display()))?;
    drop(partial);

    std::fs::rename(&paths.partial_disk, &paths.disk).with_context(|| {
        format!(
            "renaming {} to {}",
            paths.partial_disk.display(),
            paths.disk.display()
        )
    })?;

    // The rename itself has to survive a power cut, which means fsyncing the
    // directory that holds the name.
    let directory = File::open(paths.disks_directory())?;
    rustix::fs::fsync(&directory)
        .with_context(|| format!("flushing {}", paths.disks_directory().display()))?;

    ensure!(
        paths.disk.try_exists()?,
        "the disk at {} is not there after being created",
        paths.disk.display()
    );
    info!(disk = %paths.disk.display(), "disk cloned from the base image");

    Ok(())
}

/// Takes the orb's disk and its config disk away. Repeating this is a no-op.
#[instrument(skip_all)]
async fn remove_disk(paths: &Paths) -> Result<()> {
    remove_if_present(&paths.disk)?;
    remove_if_present(&paths.config_disk)?;

    ensure!(
        !paths.disk.try_exists()?,
        "the disk at {} is still there after being removed",
        paths.disk.display()
    );

    Ok(())
}

/// Spawn: leave a healthy orb alone, otherwise start it from where it is.
///
/// "Healthy" is a running VM with a byte-identical config disk. A changed
/// token means the old VM cannot talk to Amp any more, so it is stopped and
/// started again with the new one.
#[instrument(skip_all)]
async fn spawn(job: &Job, paths: &Paths) -> Result<()> {
    let wanted = ConfigDisk::new(job.token.as_ref(), job.amp_url.as_deref())?.encode()?;

    if observe(paths).await? == VmState::Running && config_disk_is(&paths.config_disk, &wanted)? {
        info!("already running with this config; leaving it alone");
        return Ok(());
    }

    stop(&job.orb, paths).await?;
    ensure_disk(paths).await?;
    start(job, paths, &wanted).await
}

/// Whether the config disk on this host already says exactly this.
///
/// Byte equality, which is what the fixed size of the config disk is for. The
/// contents are never logged: one of the values is the token.
fn config_disk_is(path: &Path, wanted: &[u8]) -> Result<bool> {
    match std::fs::read(path) {
        Ok(existing) => Ok(existing == wanted),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Starts the orb: config disk, slot, unit, VM.
///
/// Nothing is undone if any of this fails. The unit, the socket and the tap
/// that are left behind are what the next job's `recover` is for, and trying
/// to tidy up here would be a second path that a kill skips anyway.
#[instrument(skip_all, fields(orb = %job.orb))]
async fn start(job: &Job, paths: &Paths, config_disk: &[u8]) -> Result<()> {
    // Preconditions this primitive establishes for itself.
    remove_if_present(&paths.api_socket)?;
    write_config_disk(&paths.config_disk, config_disk)?;

    let slot = lowest_free_slot(&job.settings)?;
    let unit = unit_name(&job.orb);
    tracing::Span::current().record("slot", slot.index());
    info!(slot = slot.index(), unit = %unit, "starting");

    run(
        limits::SYSTEMD_RUN,
        &[
            "--no-ask-password",
            "--collect",
            &format!("--unit={unit}"),
            &format!("--slice={}", limits::SLICE),
            "-p",
            "Type=exec",
            "-p",
            &format!("TimeoutStopSec={}", limits::UNIT_STOP_TIMEOUT.as_secs()),
            "-p",
            "OOMScoreAdjust=500",
            "--",
            &job.settings.cloud_hypervisor.to_string_lossy(),
            "--api-socket",
            &paths.api_socket.to_string_lossy(),
        ],
    )
    .await
    .with_context(|| format!("starting {unit}"))?;

    wait_for_socket(&paths.api_socket).await?;

    let vm = VmBuilder::new(
        &job.settings.kernel,
        &cmdline(slot, &job.orb),
        job.settings.vcpus,
        job.settings.memory_mib,
    )
    .without_huge_pages()
    .disk(&paths.disk)
    .read_only_disk(&paths.config_disk)
    .tap(&slot.tap_name(), slot.host_address(), slot.netmask())
    .free_page_reporting()
    .boot(&paths.api_socket)
    .await
    .context("creating and booting the VM")?;

    let state = vm.state().await?;
    ensure!(
        state == VmState::Running,
        "the VM is {state:?} rather than running after being booted"
    );
    info!(slot = slot.index(), "running");

    Ok(())
}

/// The config disk, written where only root can read it. The run directory is
/// a tmpfs, so the token never reaches a disk that survives a reboot.
fn write_config_disk(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;

    file.write_all(contents)
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
    let slots = settings.slots.get().min(limits::MAX_SLOTS);

    for index in 0..slots {
        let slot = Slot::new(u8::try_from(index).context("a slot index past 255")?);
        if !Path::new(limits::SYS_CLASS_NET)
            .join(slot.tap_name())
            .exists()
        {
            return Ok(slot);
        }
    }

    bail!("no free slot: all {slots} are taken");
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
/// because `ORB_ID` reaches some of them.
async fn run(program: &str, arguments: &[&str]) -> Result<String> {
    let (status, stdout, stderr) = run_to_completion(program, arguments).await?;

    ensure!(
        status.success(),
        "{program} exited with {status}: {}",
        stderr.trim()
    );

    Ok(stdout)
}

/// Runs a program whose non-zero exit is an answer rather than a failure.
async fn run_allowing_failure(program: &str, arguments: &[&str]) -> Result<String> {
    let (_, stdout, _) = run_to_completion(program, arguments).await?;

    Ok(stdout)
}

async fn run_to_completion(
    program: &str,
    arguments: &[&str],
) -> Result<(std::process::ExitStatus, String, String)> {
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

    Ok((
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Stops the deadline from being mistaken for Amp's: ours has to be shorter.
const _: () = assert!(limits::JOB_DEADLINE.as_secs() < Duration::from_secs(300).as_secs());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_units_orb_is_the_name_between_the_prefix_and_the_suffix() {
        assert_eq!(
            orb_of_unit("jorb-amp-1.service").map(|orb| orb.to_string()),
            Some("amp-1".to_owned())
        );
    }

    #[test]
    fn a_unit_that_is_not_ours_belongs_to_no_orb() {
        for unit in [
            "sshd.service",
            "jorb-.service",
            "jorb-amp-1.slice",
            "jorb-../x.service",
            "jorb-a b.service",
        ] {
            assert!(
                orb_of_unit(unit).is_none(),
                "{unit} should not parse as an orb's unit"
            );
        }
    }

    /// The provider's own unit parses as an orb called "provider", which is
    /// why recover asks systemd for the slice as well as reading the name.
    #[test]
    fn the_providers_unit_parses_as_a_name_and_is_kept_out_by_its_slice() {
        assert_eq!(
            orb_of_unit("jorb-provider.service").map(|orb| orb.to_string()),
            Some("provider".to_owned())
        );
    }

    #[test]
    fn a_config_disk_that_is_not_there_does_not_match() {
        let missing = Path::new("/run/jorb/definitely-not-here.cfg");

        assert!(!config_disk_is(missing, b"ORB_TOKEN=x\n").unwrap());
    }

    #[test]
    fn a_config_disk_matches_only_its_own_bytes() {
        let directory = std::env::temp_dir().join("jorb-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("match.cfg");

        let wanted = ConfigDisk::new(Some(&"a-token".into()), None)
            .unwrap()
            .encode()
            .unwrap();
        write_config_disk(&path, &wanted).unwrap();

        assert!(config_disk_is(&path, &wanted).unwrap());

        let other = ConfigDisk::new(Some(&"another-token".into()), None)
            .unwrap()
            .encode()
            .unwrap();
        assert!(!config_disk_is(&path, &other).unwrap());
    }

    #[test]
    fn removing_a_path_that_is_not_there_is_not_a_failure() {
        assert!(remove_if_present(Path::new("/run/jorb/not-here-either.sock")).is_ok());
    }
}
