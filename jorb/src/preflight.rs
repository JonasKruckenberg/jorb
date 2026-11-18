//! Whether this host can run orbs, asked one question at a time.
//!
//! The gates in `tests/host/` answered these questions once, by hand, on the
//! machine jorb was written on. A host that is not that machine needs the same
//! answers, so they live here: `jorb preflight` is those gates as a command,
//! and `jorb init` ends with it.
//!
//! Every question is a syscall where it can be. Only the VMM is run, because
//! a binary that is present and will not execute is the failure worth
//! catching. Nothing here repairs anything; a failed check is a question for
//! whoever is installing jorb.
#![allow(clippy::print_stdout, reason = "the report is this module's output")]

use std::path::Path;

use anyhow::{Context, Result};

use crate::fixed;
use crate::host::{self, Host};

/// One question and what was seen.
struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
}

fn passed(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        ok: true,
        detail: detail.into(),
    }
}

fn failed(name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        ok: false,
        detail: detail.into(),
    }
}

/// What the caller may do here. Several checks can only be answered as the
/// user jorb will run as, so this is asked first and reported once rather
/// than guessed at by each of them.
pub enum Privileges {
    Root,
    /// Not root, but `sudo` will elevate without asking for a password.
    Sudo,
    Neither,
}

impl Privileges {
    pub async fn current() -> Self {
        if rustix::process::geteuid().is_root() {
            return Self::Root;
        }

        // `-n` is the only form a non-interactive program may use: it answers
        // instead of prompting.
        if host::run(fixed::SUDO, &["-n", "true"]).await.is_ok() {
            Self::Sudo
        } else {
            Self::Neither
        }
    }

    /// What to tell someone who has to be root and is not.
    pub fn advice(&self) -> &'static str {
        match self {
            Self::Root => "already root",
            Self::Sudo => "run it with sudo -E",
            Self::Neither => "sudo will not elevate without a password here, so run it as root",
        }
    }
}

/// Asks every question and reports. `Ok(false)` means the host answered one
/// of them badly; that is not this function's error to raise.
pub async fn preflight(host: &Host) -> Result<bool> {
    let privileges = Privileges::current().await;

    let checks = [
        privileges_check(&privileges),
        kvm(),
        cgroups(),
        systemd(),
        vmm(host).await,
        kernel(host),
        directory("state directory", host.state_directory()),
        directory("run directory", host.run_directory()),
        reflinks(host, &privileges),
        base_image(host),
        memory(host),
        slots(host),
    ];

    for check in &checks {
        let mark = if check.ok { "ok  " } else { "FAIL" };
        println!("{mark} {:<18} {}", check.name, check.detail);
    }

    Ok(checks.iter().all(|check| check.ok))
}

fn privileges_check(privileges: &Privileges) -> Check {
    match privileges {
        Privileges::Root => passed("privileges", "root"),
        other => failed(
            "privileges",
            format!(
                "not root, so some answers below are unknowable; {}",
                other.advice()
            ),
        ),
    }
}

/// A microVM needs the kernel's hypervisor interface, and jorb needs to open
/// it rather than merely see it.
fn kvm() -> Check {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixed::KVM_DEVICE)
    {
        Ok(_) => passed("kvm", format!("{} opens", fixed::KVM_DEVICE)),
        Err(error) => failed("kvm", format!("{}: {error}", fixed::KVM_DEVICE)),
    }
}

/// `MemoryHigh` on the slice is the host's protection against an orb taking
/// it down, and that is a cgroup v2 interface.
fn cgroups() -> Check {
    let controllers = Path::new(fixed::CGROUP_ROOT).join("cgroup.controllers");
    if controllers.exists() {
        passed("cgroup v2", controllers.display().to_string())
    } else {
        failed(
            "cgroup v2",
            format!(
                "{} is missing, so this host is on cgroup v1",
                controllers.display()
            ),
        )
    }
}

/// An orb is a systemd unit. `/run/systemd/system` answers whether systemd
/// booted this host, as against whether its binaries are installed.
fn systemd() -> Check {
    if Path::new(fixed::SYSTEMD_RUNTIME).exists() {
        passed("systemd", format!("{} is there", fixed::SYSTEMD_RUNTIME))
    } else {
        failed(
            "systemd",
            format!(
                "{} is missing, so systemd did not boot this host",
                fixed::SYSTEMD_RUNTIME
            ),
        )
    }
}

/// jorb runs the VMM by name and lets the PATH decide which one, so this runs
/// it: the failure worth catching is a binary that is there and will not
/// execute.
async fn vmm(host: &Host) -> Check {
    match host::run(&host.cloud_hypervisor.to_string_lossy(), &["--version"]).await {
        Ok(output) => passed("cloud-hypervisor", output.trim().to_owned()),
        Err(error) => failed("cloud-hypervisor", format!("{error:#}")),
    }
}

fn kernel(host: &Host) -> Check {
    match std::fs::metadata(&host.kernel) {
        Ok(metadata) if metadata.len() > 0 => passed(
            "guest kernel",
            format!("{} is {} bytes", host.kernel.display(), metadata.len()),
        ),
        Ok(_) => failed(
            "guest kernel",
            format!("{} is empty", host.kernel.display()),
        ),
        Err(error) => failed(
            "guest kernel",
            format!("{}: {error}", host.kernel.display()),
        ),
    }
}

fn directory(name: &'static str, path: &Path) -> Check {
    if path.is_dir() {
        passed(name, path.display().to_string())
    } else {
        failed(
            name,
            format!("{} is not a directory; run jorb init", path.display()),
        )
    }
}

/// Every orb's disk is a reflink clone of the base image, so a filesystem
/// that cannot clone turns each spawn into a 64 GiB copy. Asking is the only
/// way to know: ext4 and XFS look alike from outside.
fn reflinks(host: &Host, privileges: &Privileges) -> Check {
    let directory = host.state_directory();
    let filesystem = filesystem_of(directory);

    match clone_probe(host) {
        Ok(()) => passed(
            "reflinks",
            format!("{} is {filesystem}", directory.display()),
        ),
        Err(_) if !matches!(privileges, Privileges::Root) => failed(
            "reflinks",
            format!(
                "unanswered: {} is {filesystem} and this is not root",
                directory.display()
            ),
        ),
        Err(error) => failed(
            "reflinks",
            format!(
                "{} is {filesystem} and cannot clone ({error:#}); jorb init --create-state-image can give it XFS",
                directory.display()
            ),
        ),
    }
}

/// Whether the state directory can clone. ext4 refuses whatever the size, so
/// an empty file answers it.
pub fn can_clone(host: &Host) -> bool {
    clone_probe(host).is_ok()
}

fn clone_probe(host: &Host) -> Result<()> {
    let directory = host.state_directory();
    let source = directory.join(fixed::CLONE_PROBE_SOURCE);
    let clone = directory.join(fixed::CLONE_PROBE_CLONE);

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
    let Ok(mounts) = std::fs::read_to_string(fixed::MOUNTS) else {
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

fn base_image(host: &Host) -> Check {
    let image = host.base_image();
    match std::fs::metadata(&image) {
        Ok(metadata) if metadata.len() > 0 => passed(
            "base image",
            format!(
                "{} is {} MiB",
                image.display(),
                metadata.len() / 1024 / 1024
            ),
        ),
        Ok(_) => failed("base image", format!("{} is empty", image.display())),
        Err(_) => failed(
            "base image",
            format!("{} is not there; run jorb init", image.display()),
        ),
    }
}

/// Orbs are only as many as the host has memory for. This is the figure the
/// module's `memoryHigh` is set from.
fn memory(host: &Host) -> Check {
    let wanted = u64::from(host.slot_count()) * u64::from(host.memory_mib.get());

    let available = std::fs::read_to_string(fixed::MEMINFO)
        .ok()
        .and_then(|meminfo| {
            meminfo
                .lines()
                .find_map(|line| line.strip_prefix("MemAvailable:"))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|kib| kib.parse::<u64>().ok())
                .map(|kib| kib / 1024)
        });

    let Some(available) = available else {
        return failed("memory", format!("{} has no MemAvailable", fixed::MEMINFO));
    };

    let detail = format!("{available} MiB available, {wanted} MiB for every slot at once");
    if available >= wanted {
        passed("memory", detail)
    } else {
        failed("memory", detail)
    }
}

/// A slot is free when its tap is absent. A taken slot is an orb, not a
/// fault, so this never fails.
fn slots(host: &Host) -> Check {
    let free = (0..host.slot_count())
        .filter(|index| {
            !Path::new(fixed::SYS_CLASS_NET)
                .join(format!("{}{index}", fixed::TAP_PREFIX))
                .exists()
        })
        .count();

    passed("slots", format!("{free} of {} free", host.slot_count()))
}
