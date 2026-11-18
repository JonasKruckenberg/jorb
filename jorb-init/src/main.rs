//! Guest PID 1: set up what a userspace expects, hand over to `amp orb
//! bootstrap`, reap, and power off when bootstrap exits.
//!
//! It knows nothing about Amp or the host; the config disk on `/dev/vdb` is
//! the whole interface. A running orb means bootstrap is alive: any failure
//! before it starts, and bootstrap exiting, end in a power off, so the host
//! sees the unit stop. Pause is a power cut; ext4 replays its journal.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use rustix::mount::{MountFlags, mount};
use rustix::process::{WaitOptions, waitpid};
use rustix::system::{RebootCommand, reboot};
use tracing::{error, info};

const CONFIG_DISK: &str = "/dev/vdb";
/// The image's own environment, written by jorb-prepare-image.
const IMAGE_ENVIRONMENT: &str = "/etc/jorb-env";
/// Pause is a power cut, so dirty pages wait a second, not thirty.
const DIRTY_CENTISECS: &str = "100";
const DEV_SHM_OPTIONS: &CStr = c"size=1G";
const USER: &str = "user";
const HOME: &str = "/home/user";
const SETPRIV: &str = "/usr/bin/setpriv";
const AMP: &str = "/usr/local/bin/amp";

fn main() -> ! {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .init();

    match start_bootstrap() {
        Ok(bootstrap) => reap(bootstrap),
        Err(error) => {
            error!(error = format!("{error:#}"), "setup failed, powering off");
            power_off()
        }
    }
}

fn start_bootstrap() -> Result<u32> {
    // devpts keeps device nodes, so a pty can be opened by its path.
    let ordinary = MountFlags::NOSUID | MountFlags::NODEV;
    for (filesystem, target, flags, options) in [
        ("proc", "/proc", ordinary, None),
        ("sysfs", "/sys", ordinary, None),
        ("devpts", "/dev/pts", MountFlags::NOSUID, None),
        ("tmpfs", "/dev/shm", ordinary, Some(DEV_SHM_OPTIONS)),
        ("tmpfs", "/run", ordinary, None),
    ] {
        fs::create_dir_all(target)?;
        mount(filesystem, target, filesystem, flags, options)
            .with_context(|| format!("mounting {target}"))?;
    }

    // devtmpfs does not create these, and `<(...)` and `> /dev/stderr` need them.
    for (link, target) in [
        ("/dev/fd", "/proc/self/fd"),
        ("/dev/stdin", "/proc/self/fd/0"),
        ("/dev/stdout", "/proc/self/fd/1"),
        ("/dev/stderr", "/proc/self/fd/2"),
    ] {
        symlink(target, link).with_context(|| format!("linking {link}"))?;
    }

    for knob in ["dirty_expire_centisecs", "dirty_writeback_centisecs"] {
        fs::write(format!("/proc/sys/vm/{knob}"), DIRTY_CENTISECS)?;
    }

    // The kernel's `ip=` leaves the resolvers in resolv.conf's own format, and
    // sets the hostname to the orb id, which DNS cannot answer for.
    // Read, not fs::copy: procfs reports a size of 0.
    fs::write("/etc/resolv.conf", fs::read("/proc/net/pnp")?)
        .context("writing /etc/resolv.conf")?;
    let hostname = fs::read_to_string("/proc/sys/kernel/hostname")?;
    fs::write("/etc/hosts", hosts(hostname.trim())).context("writing /etc/hosts")?;

    let environment = environment(&fs::read(IMAGE_ENVIRONMENT)?, &fs::read(CONFIG_DISK)?)?;
    let bootstrap = Command::new(SETPRIV)
        .args([
            &format!("--reuid={USER}"),
            &format!("--regid={USER}"),
            "--init-groups",
            "--",
        ])
        .args([AMP, "orb", "bootstrap"])
        .current_dir(HOME)
        .env_clear()
        .envs(environment)
        .spawn()
        .context("spawning bootstrap")?;
    info!(pid = bootstrap.id(), "bootstrap started");
    Ok(bootstrap.id())
}

fn hosts(hostname: &str) -> String {
    format!(
        "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n127.0.0.2\t{hostname}\n"
    )
}

/// The image's variables, then who the guest runs as, then the config disk,
/// which wins because it is what the host said about this orb.
fn environment(image: &[u8], config_disk: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut environment: BTreeMap<_, _> = jorb_config_disk::parse(image)?.into_iter().collect();
    environment.insert("HOME".into(), HOME.into());
    environment.insert("USER".into(), USER.into());
    environment.extend(jorb_config_disk::parse(config_disk)?);
    ensure!(
        environment.contains_key("ORB_TOKEN"),
        "the config disk has no ORB_TOKEN"
    );
    Ok(environment)
}

/// Reaps every orphan until bootstrap exits. The one loop without a visible
/// bound: it ends with bootstrap, and while bootstrap is a child `waitpid`
/// always has someone to wait for.
fn reap(bootstrap: u32) -> ! {
    loop {
        match waitpid(None, WaitOptions::empty()) {
            Ok(Some((pid, status))) if pid.as_raw_nonzero().get().cast_unsigned() == bootstrap => {
                info!(
                    exit_status = status.exit_status(),
                    signal = status.terminating_signal(),
                    "bootstrap exited, powering off"
                );
                power_off()
            }
            Ok(Some(_)) | Err(rustix::io::Errno::INTR) => (),
            other => {
                error!(result = ?other, "waiting for children failed");
                power_off()
            }
        }
    }
}

fn power_off() -> ! {
    rustix::fs::sync();
    if let Err(error) = reboot(RebootCommand::PowerOff) {
        error!(error = error.to_string(), "powering off failed");
    }
    // Reached only if powering off failed. Exiting panics the kernel, which
    // halts without `panic=`, so the unit stays active until the host stops it.
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(entries: &[(&str, &str)]) -> Vec<u8> {
        jorb_config_disk::ConfigDisk::encode(entries)
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn the_config_disk_wins_and_must_carry_a_token() {
        let image = b"PATH=/usr/bin:/bin\nAMP_URL=https://image.test\nORB_TOKEN=from-image\n";
        let built = environment(
            image,
            &disk(&[("ORB_TOKEN", "from-host"), ("AMP_URL", "https://host.test")]),
        )
        .unwrap();

        for (key, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("ORB_TOKEN", "from-host"),
            ("AMP_URL", "https://host.test"),
            ("HOME", "/home/user"),
            ("USER", "user"),
        ] {
            assert_eq!(built[key], value);
        }

        let refusal = environment(b"PATH=/bin\n", &disk(&[("AMP_URL", "x")])).unwrap_err();
        assert!(refusal.to_string().contains("ORB_TOKEN"));
    }

    #[test]
    fn the_hostname_resolves_to_its_own_loopback_address() {
        assert!(hosts("amp-1").ends_with("\n127.0.0.2\tamp-1\n"));
    }
}
