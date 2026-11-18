//! Guest PID 1: mount, settle the writeback window, resolve DNS, build the
//! environment, hand over to `amp orb bootstrap`, reap, and power off when
//! bootstrap exits.
//!
//! It knows nothing about Amp's job protocol and nothing about the host. The
//! config disk on `/dev/vdb` is the whole interface between the two: jorb
//! writes it, this reads it, and neither needs to agree on anything else.
//!
//! A running orb means bootstrap is alive. Every failure before bootstrap
//! starts ends in a power off, and so does bootstrap exiting: a guest without
//! it has nothing to offer, and a stopped unit is the truthful state for the
//! host to see. Pause is a power cut, and ext4 replays its journal on the
//! next boot.

mod constants;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use rustix::mount::{MountFlags, mount};
use rustix::process::{WaitOptions, waitpid};
use rustix::system::{RebootCommand, reboot};
use tracing::{error, info};

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

/// Everything that has to be true before bootstrap starts, in order, then
/// bootstrap itself. Returns its pid: the one child reap reports on.
fn start_bootstrap() -> Result<u32> {
    mount_filesystems()?;
    link_standard_streams()?;
    shorten_the_writeback_window()?;
    write_resolv_conf()?;
    write_hosts()?;

    let environment = environment(
        Path::new(constants::ENV_FILE),
        Path::new(constants::CONFIG_DISK_DEVICE),
    )?;

    spawn_bootstrap(&environment)
}

/// The kernel mounts devtmpfs itself; these are the rest of what a userspace
/// expects to find. devpts is the one without `nodev`: its entries are the
/// pty devices, and a program opening `/dev/pts/N` by path needs them to be.
fn mount_filesystems() -> Result<()> {
    let ordinary = MountFlags::NOSUID | MountFlags::NODEV;

    for (source, target, filesystem, flags, options) in [
        ("proc", "/proc", "proc", ordinary, None),
        ("sysfs", "/sys", "sysfs", ordinary, None),
        ("devpts", "/dev/pts", "devpts", MountFlags::NOSUID, None),
        (
            "tmpfs",
            "/dev/shm",
            "tmpfs",
            ordinary,
            Some(constants::DEV_SHM_OPTIONS),
        ),
        ("tmpfs", "/run", "tmpfs", ordinary, None),
    ] {
        fs::create_dir_all(target).with_context(|| format!("creating {target}"))?;

        mount(source, target, filesystem, flags, options)
            .with_context(|| format!("mounting {filesystem} on {target}"))?;
    }

    Ok(())
}

/// `/dev/fd` and the three standard streams, which devtmpfs does not create
/// and which `<(...)` and `> /dev/stderr` rely on.
fn link_standard_streams() -> Result<()> {
    for (link, target) in [
        ("/dev/fd", "/proc/self/fd"),
        ("/dev/stdin", "/proc/self/fd/0"),
        ("/dev/stdout", "/proc/self/fd/1"),
        ("/dev/stderr", "/proc/self/fd/2"),
    ] {
        std::os::unix::fs::symlink(target, link)
            .with_context(|| format!("linking {link} to {target}"))?;
    }

    Ok(())
}

/// Pause is a power cut, so the guest should not sit on dirty pages for the
/// kernel's usual thirty seconds. This cuts the window to about a second.
fn shorten_the_writeback_window() -> Result<()> {
    for knob in [
        "/proc/sys/vm/dirty_expire_centisecs",
        "/proc/sys/vm/dirty_writeback_centisecs",
    ] {
        fs::write(knob, constants::DIRTY_WRITEBACK_CENTISECS.to_string())
            .with_context(|| format!("writing {knob}"))?;
    }

    Ok(())
}

/// The kernel's ip= cmdline leaves what it learned in /proc/net/pnp, in
/// resolv.conf's own format.
fn write_resolv_conf() -> Result<()> {
    let resolved = fs::read_to_string("/proc/net/pnp").context("reading /proc/net/pnp")?;
    fs::write("/etc/resolv.conf", resolved).context("writing /etc/resolv.conf")
}

/// The guest's hostname is its orb id, set by the kernel from `ip=`. Writing
/// it to `/etc/hosts` makes it resolve without DNS, which cannot answer for
/// it.
fn write_hosts() -> Result<()> {
    let hostname = fs::read_to_string(constants::HOSTNAME).context("reading the hostname")?;
    fs::write(constants::HOSTS, hosts(hostname.trim())).context("writing /etc/hosts")
}

/// `127.0.0.2` keeps the hostname off the `localhost` line, as Debian does.
fn hosts(hostname: &str) -> String {
    format!(
        "127.0.0.1\tlocalhost\n\
         ::1\tlocalhost ip6-localhost ip6-loopback\n\
         127.0.0.2\t{hostname}\n"
    )
}

/// The environment bootstrap runs with: the image's own variables, then who
/// the guest runs as, then the config disk, which wins because it is the one
/// thing the host said about this orb.
///
/// A missing env file is a failure. jorb-prepare-image always writes one, so
/// its absence means this is not the image jorb built, and booting on without
/// the image's PATH would only fail further in.
fn environment(env_file: &Path, config_disk: &Path) -> Result<BTreeMap<String, String>> {
    let image =
        fs::read_to_string(env_file).with_context(|| format!("reading {}", env_file.display()))?;
    let mut environment = parse_env_file(&image);

    environment.insert("HOME".to_owned(), constants::GUEST_HOME.to_owned());
    environment.insert("USER".to_owned(), constants::GUEST_USER.to_owned());

    let disk =
        fs::read(config_disk).with_context(|| format!("reading {}", config_disk.display()))?;
    environment.extend(jorb_config_disk::parse(&disk)?);

    if !environment.contains_key("ORB_TOKEN") {
        bail!("the config disk has no ORB_TOKEN, so bootstrap has nothing to connect with");
    }

    Ok(environment)
}

/// `KEY=VALUE` lines, as the image's own `Env` was written out. Blank lines
/// and comments are skipped, and so is anything without a `=`: an image that
/// writes something else is not describing an environment.
fn parse_env_file(contents: &str) -> BTreeMap<String, String> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Hands over to bootstrap as the image's unprivileged user, in its home.
///
/// The child keeps running after this returns: it is the guest's whole
/// purpose, and PID 1 only reaps from here on.
fn spawn_bootstrap(environment: &BTreeMap<String, String>) -> Result<u32> {
    let child = Command::new(constants::SETPRIV)
        .args([
            &format!("--reuid={}", constants::GUEST_USER),
            &format!("--regid={}", constants::GUEST_USER),
            "--init-groups",
            "--",
            constants::AMP,
            "orb",
            "bootstrap",
        ])
        .current_dir(constants::GUEST_HOME)
        .env_clear()
        .envs(environment)
        .spawn()
        .with_context(|| format!("spawning {} through {}", constants::AMP, constants::SETPRIV))?;

    info!(
        pid = child.id(),
        user = constants::GUEST_USER,
        "bootstrap started"
    );

    Ok(child.id())
}

/// Reaps children until bootstrap exits, then powers off.
///
/// PID 1 inherits every orphan in the guest, so this reaps whatever exits.
/// The one loop in jorb without a visible bound: it ends when bootstrap does,
/// which is why this function returns `!`. With bootstrap still a child,
/// `waitpid` always has someone to wait for, so any error is unexpected.
fn reap(bootstrap: u32) -> ! {
    loop {
        match waitpid(None, WaitOptions::empty()) {
            Ok(Some((pid, status))) if pid.as_raw_nonzero().get().cast_unsigned() == bootstrap => {
                info!(
                    pid = bootstrap,
                    exit_status = status.exit_status(),
                    signal = status.terminating_signal(),
                    "bootstrap exited, powering off"
                );
                power_off()
            }
            Ok(Some(_)) | Err(rustix::io::Errno::INTR) => (),
            Ok(None) => {
                error!("waiting for children returned none, which it cannot without WNOHANG");
                power_off()
            }
            Err(error) => {
                error!(error = error.to_string(), "waiting for children failed");
                power_off()
            }
        }
    }
}

/// Flushes and switches the guest off. The host sees the unit stop.
fn power_off() -> ! {
    rustix::fs::sync();

    if let Err(error) = reboot(RebootCommand::PowerOff) {
        error!(error = error.to_string(), "powering off failed");
    }

    // The syscall does not return when it works. If it failed, exiting makes
    // the kernel panic, and with no `panic=` on the cmdline the guest then
    // halts: the unit stays active until the host stops it.
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own, so two tests cannot tread on each
    /// other's files.
    fn test_directory(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join("jorb-init-tests").join(name);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    /// A config disk as jorb writes it.
    fn config_disk(entries: &[(&str, &str)]) -> Vec<u8> {
        jorb_config_disk::ConfigDisk::encode(entries)
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn env_file_lines_become_variables() {
        let parsed = parse_env_file(
            "PATH=/usr/bin:/bin\n\
             \n\
             # a comment\n\
             LANG=C.UTF-8\n\
             NOT_AN_ASSIGNMENT\n\
             EMPTY=\n\
             AMP_URL=https://example.test/?a=b\n",
        );

        assert_eq!(
            parsed.get("PATH").map(String::as_str),
            Some("/usr/bin:/bin")
        );
        assert_eq!(parsed.get("LANG").map(String::as_str), Some("C.UTF-8"));
        assert_eq!(parsed.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(
            parsed.get("AMP_URL").map(String::as_str),
            Some("https://example.test/?a=b"),
            "a value may contain an equals sign"
        );
        assert!(!parsed.contains_key("NOT_AN_ASSIGNMENT"));
        assert_eq!(parsed.len(), 4);
    }

    #[test]
    fn the_hostname_resolves_to_its_own_loopback_address() {
        assert_eq!(
            hosts("amp-T-1a2b"),
            "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n127.0.0.2\tamp-T-1a2b\n"
        );
    }

    #[test]
    fn a_config_disk_without_a_token_is_refused() {
        let directory = test_directory("no-token");
        let env_path = directory.join("jorb-env");
        let disk_path = directory.join("no-token.cfg");
        fs::write(&env_path, "PATH=/usr/bin\n").unwrap();
        fs::write(&disk_path, config_disk(&[("AMP_URL", "https://a.test")])).unwrap();

        let error = environment(&env_path, &disk_path)
            .expect_err("a config disk without a token is refused");

        assert!(error.to_string().contains("ORB_TOKEN"));
    }

    #[test]
    fn the_config_disk_wins_over_the_image_environment() {
        let directory = test_directory("precedence");
        let env_path = directory.join("jorb-env");
        let disk_path = directory.join("with-token.cfg");
        fs::write(
            &env_path,
            "AMP_URL=https://image.test\nORB_TOKEN=from-image\n",
        )
        .unwrap();
        fs::write(
            &disk_path,
            config_disk(&[("ORB_TOKEN", "from-host"), ("AMP_URL", "https://host.test")]),
        )
        .unwrap();

        let environment = environment(&env_path, &disk_path).expect("the environment builds");

        assert_eq!(
            environment.get("ORB_TOKEN").map(String::as_str),
            Some("from-host")
        );
        assert_eq!(
            environment.get("AMP_URL").map(String::as_str),
            Some("https://host.test")
        );
        assert_eq!(
            environment.get("HOME").map(String::as_str),
            Some("/home/user")
        );
        assert_eq!(environment.get("USER").map(String::as_str), Some("user"));
    }

    /// A missing env file means the image is not the one jorb-prepare-image
    /// made, which jorb-init refuses instead of booting on without a PATH.
    #[test]
    fn a_missing_env_file_is_refused() {
        let directory = test_directory("no-env-file");
        let disk_path = directory.join("with-token.cfg");
        fs::write(&disk_path, config_disk(&[("ORB_TOKEN", "secret")])).unwrap();

        let error = environment(&directory.join("not-here"), &disk_path)
            .expect_err("a missing env file is refused");

        assert!(format!("{error:#}").contains("not-here"));
    }
}
