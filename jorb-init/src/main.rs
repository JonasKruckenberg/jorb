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

use anyhow::{Context, Result};
use jorb_config_disk::ConfigDisk;
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
    // ===== Filesystems: what a userspace expects beyond the kernel's devtmpfs =====

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

    // ===== Writeback: pause is a power cut, so flush within a second =====

    for knob in ["dirty_expire_centisecs", "dirty_writeback_centisecs"] {
        fs::write(format!("/proc/sys/vm/{knob}"), DIRTY_CENTISECS)?;
    }

    // ===== Names: resolvers and the orb's own hostname =====

    // The kernel's `ip=` leaves the resolvers in resolv.conf's own format, and
    // sets the hostname to the orb id, which DNS cannot answer for.
    // Read, not fs::copy: procfs reports a size of 0.
    fs::write("/etc/resolv.conf", fs::read("/proc/net/pnp")?)
        .context("writing /etc/resolv.conf")?;
    let hostname = fs::read_to_string("/proc/sys/kernel/hostname")?;
    fs::write("/etc/hosts", hosts(hostname.trim())).context("writing /etc/hosts")?;

    // ===== Bootstrap: the image's environment plus the config disk, as `user` =====

    let config_disk =
        ConfigDisk::read_from(fs::File::open(CONFIG_DISK)?).context("reading the config disk")?;
    let environment = environment(&fs::read_to_string(IMAGE_ENVIRONMENT)?, &config_disk);
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

/// The image's `KEY=VALUE` lines, then who the guest runs as, then the config
/// disk, which wins because it is what the host said about this orb.
fn environment(image: &str, config_disk: &ConfigDisk) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<String, String> = image
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    environment.insert("HOME".into(), HOME.into());
    environment.insert("USER".into(), USER.into());
    environment.insert("ORB_TOKEN".into(), config_disk.orb_token.clone());
    if let Some(amp_url) = &config_disk.amp_url {
        environment.insert("AMP_URL".into(), amp_url.clone());
    }
    environment
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

    #[test]
    fn the_config_disk_wins_over_the_image() {
        let image = "PATH=/usr/bin:/bin\nAMP_URL=https://image.test\nORB_TOKEN=from-image\n";
        let config_disk = ConfigDisk {
            orb_token: "from-host".into(),
            amp_url: Some("https://host.test".into()),
        };
        let built = environment(image, &config_disk);

        for (key, value) in [
            ("PATH", "/usr/bin:/bin"),
            ("ORB_TOKEN", "from-host"),
            ("AMP_URL", "https://host.test"),
            ("HOME", "/home/user"),
            ("USER", "user"),
        ] {
            assert_eq!(built[key], value);
        }
    }

    #[test]
    fn the_hostname_resolves_to_its_own_loopback_address() {
        assert!(hosts("amp-1").ends_with("\n127.0.0.2\tamp-1\n"));
    }
}
