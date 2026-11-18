//! Whether this host can run orbs, asked one question at a time.
//!
//! The gates in `tests/host/` answered these questions once, by hand, on the
//! machine jorb was written on. A host that is not that machine needs the same
//! answers, so they live here: `jorb preflight` is those gates as a command,
//! and `jorb init` ends with it.
//!
//! A question is a syscall where it can be. Only the VMM is executed, because
//! a binary that is present and will not run is the failure worth catching.
//! Nothing here repairs anything: a failed question is for whoever is
//! installing jorb to answer.
//!
//! Each question returns the evidence for its answer, or the error that is its
//! answer. There is no reporting type beyond that.
#![allow(clippy::print_stdout, reason = "the report is this module's output")]

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

use crate::constants;
use crate::host::{self, HostConfig, Slot};

/// Asks every question and prints the answers. `Ok(false)` means the host
/// answered one of them badly; that is not this function's error to raise.
pub async fn preflight(host: &HostConfig) -> Result<bool> {
    let root = root().await;
    let as_root = root.is_ok();

    let answers = [
        ("privileges", root),
        ("kvm", kvm()),
        ("cgroup v2", cgroups()),
        ("systemd", systemd()),
        ("cloud-hypervisor", vmm(host).await),
        ("guest kernel", kernel(host)),
        ("state directory", directory(host.state_directory())),
        ("run directory", directory(host.run_directory())),
        ("reflinks", reflinks(host, as_root)),
        ("base image", base_image(host)),
        ("memory", memory(host)),
        ("slots", slots(host)),
    ];

    let mut ready = true;
    for (question, answer) in &answers {
        match answer {
            Ok(evidence) => println!("ok   {question:<18} {evidence}"),
            Err(reason) => {
                ready = false;
                println!("FAIL {question:<18} {reason:#}");
            }
        }
    }

    Ok(ready)
}

/// Whether this process can do what jorb does to a host, and if not, how to
/// become something that can. `jorb init` asks the same question.
pub async fn root() -> Result<String> {
    if rustix::process::geteuid().is_root() {
        return Ok("root".to_owned());
    }

    // `-n` is the only form a non-interactive program may use: it answers
    // instead of prompting.
    if host::run(constants::SUDO, &["-n", "true"]).await.is_ok() {
        bail!("not root; run it with sudo -E");
    }

    bail!("not root, and sudo will not elevate without a password here; run it as root");
}

/// A microVM needs the kernel's hypervisor interface, and jorb needs to open
/// it rather than merely see it.
fn kvm() -> Result<String> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(constants::KVM_DEVICE)
        .with_context(|| format!("opening {}", constants::KVM_DEVICE))?;

    Ok(format!("{} opens", constants::KVM_DEVICE))
}

/// `MemoryHigh` on the slice is the host's protection against an orb taking
/// it down, and that is a cgroup v2 interface.
fn cgroups() -> Result<String> {
    let controllers = Path::new(constants::CGROUP_ROOT).join("cgroup.controllers");
    ensure!(
        controllers.exists(),
        "{} is missing, so this host is on cgroup v1",
        controllers.display()
    );

    Ok(controllers.display().to_string())
}

/// An orb is a systemd unit. `/run/systemd/system` answers whether systemd
/// booted this host, as against whether its binaries are installed.
fn systemd() -> Result<String> {
    ensure!(
        Path::new(constants::SYSTEMD_RUNTIME).exists(),
        "{} is missing, so systemd did not boot this host",
        constants::SYSTEMD_RUNTIME
    );

    Ok(format!("{} is there", constants::SYSTEMD_RUNTIME))
}

/// jorb runs the VMM by name and lets the PATH decide which one, so this runs
/// it: the failure worth catching is a binary that is there and will not
/// execute.
async fn vmm(host: &HostConfig) -> Result<String> {
    let version = host::run(&host.cloud_hypervisor.to_string_lossy(), &["--version"]).await?;

    Ok(version.trim().to_owned())
}

fn kernel(host: &HostConfig) -> Result<String> {
    let size = file_size(&host.kernel)?;

    Ok(format!("{} is {size} bytes", host.kernel.display()))
}

fn base_image(host: &HostConfig) -> Result<String> {
    let image = host.base_image();
    let size = file_size(&image).context("run jorb init")?;

    Ok(format!("{} is {} MiB", image.display(), size / 1024 / 1024))
}

fn file_size(path: &Path) -> Result<u64> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .len();
    ensure!(size > 0, "{} is empty", path.display());

    Ok(size)
}

fn directory(path: &Path) -> Result<String> {
    ensure!(
        path.is_dir(),
        "{} is not a directory; run jorb init",
        path.display()
    );

    Ok(path.display().to_string())
}

/// Every orb's disk is a reflink clone of the base image, so a filesystem
/// that cannot clone turns each spawn into a 64 GiB copy. Asking is the only
/// way to know: ext4 and XFS look alike from outside.
fn reflinks(host: &HostConfig, as_root: bool) -> Result<String> {
    let directory = host.state_directory();
    let filesystem = filesystem_of(directory);

    match clone_probe(host) {
        Ok(()) => Ok(format!("{} is {filesystem}", directory.display())),
        // Without the privileges to write there, the answer is unknown, and
        // blaming the filesystem would send someone to repartition a disk
        // that was fine.
        Err(_) if !as_root => bail!(
            "unanswered: {} is {filesystem} and this is not root",
            directory.display()
        ),
        Err(error) => Err(error).with_context(|| {
            format!(
                "{} is {filesystem} and cannot clone; jorb init --create-state-image can give it XFS",
                directory.display()
            )
        }),
    }
}

/// Whether the state directory can clone. ext4 refuses whatever the size, so
/// an empty file answers it.
pub fn can_clone(host: &HostConfig) -> bool {
    clone_probe(host).is_ok()
}

fn clone_probe(host: &HostConfig) -> Result<()> {
    let directory = host.state_directory();
    let source = directory.join(constants::CLONE_PROBE_SOURCE);
    let clone = directory.join(constants::CLONE_PROBE_CLONE);

    let from =
        std::fs::File::create(&source).with_context(|| format!("creating {}", source.display()))?;
    let to =
        std::fs::File::create(&clone).with_context(|| format!("creating {}", clone.display()))?;
    let outcome = rustix::fs::ioctl_ficlone(&to, &from).context("cloning");

    drop(from);
    drop(to);
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&clone);

    outcome
}

/// The filesystem a path is on, from `/proc/mounts`. The longest mount point
/// the path sits under wins, so a nested mount answers for itself.
fn filesystem_of(path: &Path) -> String {
    let unknown = || "an unknown filesystem".to_owned();
    let Ok(mounts) = std::fs::read_to_string(constants::MOUNTS) else {
        return unknown();
    };

    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (_device, mount_point, kind) = (fields.next()?, fields.next()?, fields.next()?);
            path.starts_with(mount_point)
                .then_some((mount_point.len(), kind))
        })
        .max_by_key(|(length, _)| *length)
        .map_or_else(unknown, |(_, kind)| kind.to_owned())
}

/// Orbs are only as many as the host has memory for. This is the figure the
/// module's `memoryHigh` is set from.
fn memory(host: &HostConfig) -> Result<String> {
    let wanted = u64::from(host.slot_count()) * u64::from(host.memory_mib.get());

    let meminfo = std::fs::read_to_string(constants::MEMINFO)
        .with_context(|| format!("reading {}", constants::MEMINFO))?;
    let available = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kib| kib.parse::<u64>().ok())
        .map(|kib| kib / 1024)
        .with_context(|| format!("{} has no MemAvailable", constants::MEMINFO))?;

    ensure!(
        available >= wanted,
        "{available} MiB available, {wanted} MiB for every slot at once"
    );

    Ok(format!(
        "{available} MiB available, {wanted} MiB for every slot at once"
    ))
}

/// A slot is free when its tap is absent. A taken slot is an orb, not a
/// fault, so this cannot fail.
fn slots(host: &HostConfig) -> Result<String> {
    let free = Slot::all(host).filter(|slot| slot.is_free()).count();

    Ok(format!("{free} of {} free", host.slot_count()))
}
