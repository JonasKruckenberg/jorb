//! Guest PID 1: mount, settle the writeback window, resolve DNS, build the
//! environment, hand over to `amp orb bootstrap`, then reap for the life of
//! the guest.
//!
//! It knows nothing about Amp's job protocol and nothing about the host. The
//! config disk on `/dev/vdb` is the whole interface between the two: jorb
//! writes it, this reads it, and neither needs to agree on anything else.
//!
//! Every failure before bootstrap starts ends in a power off rather than a
//! retry. A guest that cannot run bootstrap has nothing to offer, and a
//! stopped unit is the truthful state for the host to see. There is no
//! shutdown path: pause is a power cut, and ext4 replays its journal on the
//! next boot.

mod limits;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use rustix::mount::{MountFlags, mount};
use rustix::process::{WaitOptions, waitpid};
use rustix::system::{RebootCommand, reboot};
use tracing::{error, info, warn};

fn main() -> ! {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .init();

    match setup() {
        Ok(bootstrap) => reap(bootstrap),
        Err(error) => {
            error!(error = format!("{error:#}"), "setup failed, powering off");
            power_off()
        }
    }
}

/// Everything that has to be true before bootstrap starts, in order. Returns
/// the bootstrap process, which is the only thing the guest exists to run.
fn setup() -> Result<std::process::Child> {
    mount_filesystems()?;
    shorten_the_writeback_window()?;
    write_resolv_conf()?;

    let environment = environment(
        Path::new(limits::ENV_FILE),
        Path::new(limits::CONFIG_DISK_DEVICE),
    )?;

    start_bootstrap(&environment)
}

/// The kernel mounts devtmpfs itself; these are the rest of what a userspace
/// expects to find.
fn mount_filesystems() -> Result<()> {
    let shm_size = format!("size={}", limits::DEV_SHM_SIZE);

    for (source, target, filesystem, data) in [
        ("proc", "/proc", "proc", None),
        ("sysfs", "/sys", "sysfs", None),
        ("devpts", "/dev/pts", "devpts", None),
        ("tmpfs", "/dev/shm", "tmpfs", Some(shm_size.as_str())),
        ("tmpfs", "/run", "tmpfs", None),
    ] {
        fs::create_dir_all(target).with_context(|| format!("creating {target}"))?;

        let flags = MountFlags::NOSUID | MountFlags::NODEV;
        let options = data.map(std::ffi::CString::new).transpose()?;

        mount(
            source,
            target,
            filesystem,
            flags,
            options.as_ref().map(AsRef::as_ref),
        )
        .with_context(|| format!("mounting {filesystem} on {target}"))?;
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
        fs::write(knob, limits::DIRTY_WRITEBACK_CENTISECS.to_string())
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

/// The environment bootstrap runs with: the image's own variables, then who
/// the guest runs as, then the config disk, which wins because it is the one
/// thing the host said about this orb.
fn environment(env_file: &Path, config_disk: &Path) -> Result<BTreeMap<String, String>> {
    let mut environment = parse_env_file(&fs::read_to_string(env_file).unwrap_or_default());

    environment.insert("HOME".to_owned(), limits::GUEST_HOME.to_owned());
    environment.insert("USER".to_owned(), limits::GUEST_USER.to_owned());

    let disk =
        fs::read(config_disk).with_context(|| format!("reading {}", config_disk.display()))?;
    environment.extend(parse_config_disk(&disk)?);

    if !environment.contains_key("ORB_TOKEN") {
        bail!("the config disk has no ORB_TOKEN, so bootstrap has nothing to connect with");
    }

    Ok(environment)
}

/// `KEY=VALUE` lines, as the image's own `Env` was written out. Blank lines
/// and comments are skipped; so is anything without a `=`, because an image
/// that writes something else is not describing an environment.
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

/// The config disk: `KEY=VALUE` lines in the first block, NUL-padded to the
/// end. Anything after the padding is not ours to read.
fn parse_config_disk(disk: &[u8]) -> Result<BTreeMap<String, String>> {
    let end = disk
        .iter()
        .take(limits::CONFIG_DISK_BYTES)
        .position(|byte| *byte == 0)
        .unwrap_or(disk.len().min(limits::CONFIG_DISK_BYTES));

    let contents = std::str::from_utf8(&disk[..end]).context("the config disk is not UTF-8")?;

    Ok(parse_env_file(contents))
}

/// Hands over to bootstrap as the image's unprivileged user, in its home.
///
/// The child keeps running after this returns: it is the guest's whole
/// purpose, and PID 1 only reaps from here on.
fn start_bootstrap(environment: &BTreeMap<String, String>) -> Result<std::process::Child> {
    let mut command = Command::new(limits::SETPRIV);
    command
        .args([
            &format!("--reuid={}", limits::GUEST_USER),
            &format!("--regid={}", limits::GUEST_USER),
            "--init-groups",
            "--",
            limits::AMP,
            "orb",
            "bootstrap",
        ])
        .current_dir(limits::GUEST_HOME)
        .env_clear()
        .envs(environment);

    let child = command
        .spawn()
        .with_context(|| format!("spawning {} through {}", limits::AMP, limits::SETPRIV))?;

    info!(
        pid = child.id(),
        user = limits::GUEST_USER,
        "bootstrap started"
    );

    Ok(child)
}

/// Reaps children for the life of the guest.
///
/// The one unbounded loop in jorb, and the reason this function returns `!`:
/// PID 1 inherits every orphan in the guest, and a guest ends by being
/// switched off rather than by this returning. Bootstrap exiting is logged and
/// nothing else: the orb stays up until the host stops it, which is what
/// `pause` does.
fn reap(bootstrap: std::process::Child) -> ! {
    let bootstrap = bootstrap.id();

    loop {
        match waitpid(None, WaitOptions::empty()) {
            Ok(Some((pid, status))) => {
                if pid.as_raw_nonzero().get().cast_unsigned() == bootstrap {
                    info!(
                        pid = bootstrap,
                        exit_status = status.exit_status(),
                        "bootstrap exited"
                    );
                }
            }
            // No children left at all. Nothing can arrive after this, so
            // waiting again would spin.
            Ok(None) => {
                warn!("no children left to reap");
                power_off()
            }
            Err(rustix::io::Errno::INTR) => (),
            Err(error) => {
                error!(error = error.to_string(), "waiting for children failed");
                power_off()
            }
        }
    }
}

/// Switches the guest off. The host sees the unit stop, which is the truthful
/// state when the guest has nothing left to do.
fn power_off() -> ! {
    if let Err(error) = reboot(RebootCommand::PowerOff) {
        error!(error = error.to_string(), "powering off failed");
    }

    // The reboot syscall does not return when it works. If it did fail, there
    // is nothing further to try: stop asking the kernel to do things.
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_lines_become_variables() {
        let parsed = parse_env_file(
            "PATH=/usr/bin:/bin\n\
             \n\
             # a comment\n\
             LANG=C.UTF-8\n\
             NOT_AN_ASSIGNMENT\n\
             EMPTY=\n",
        );

        assert_eq!(
            parsed.get("PATH").map(String::as_str),
            Some("/usr/bin:/bin")
        );
        assert_eq!(parsed.get("LANG").map(String::as_str), Some("C.UTF-8"));
        assert_eq!(parsed.get("EMPTY").map(String::as_str), Some(""));
        assert!(!parsed.contains_key("NOT_AN_ASSIGNMENT"));
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn a_value_may_contain_an_equals_sign() {
        let parsed = parse_env_file("AMP_URL=https://example.test/?a=b\n");

        assert_eq!(
            parsed.get("AMP_URL").map(String::as_str),
            Some("https://example.test/?a=b")
        );
    }

    /// A config disk as jorb writes it: lines, then NUL padding to the block.
    fn config_disk(contents: &str) -> Vec<u8> {
        let mut disk = contents.as_bytes().to_vec();
        disk.resize(limits::CONFIG_DISK_BYTES, 0);
        disk
    }

    #[test]
    fn padding_is_not_part_of_the_config() {
        let parsed = parse_config_disk(&config_disk("ORB_TOKEN=secret\nAMP_URL=https://a.test\n"))
            .expect("the disk parses");

        assert_eq!(parsed.get("ORB_TOKEN").map(String::as_str), Some("secret"));
        assert_eq!(
            parsed.get("AMP_URL").map(String::as_str),
            Some("https://a.test")
        );
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn an_empty_config_disk_parses_to_nothing() {
        let parsed = parse_config_disk(&config_disk("")).expect("the disk parses");

        assert!(parsed.is_empty());
    }

    #[test]
    fn blank_lines_in_the_config_disk_are_skipped() {
        let parsed =
            parse_config_disk(&config_disk("\n\nORB_TOKEN=secret\n\n")).expect("the disk parses");

        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn nothing_past_the_first_block_is_read() {
        let mut disk = config_disk("ORB_TOKEN=secret\n");
        disk.extend_from_slice(b"ORB_TOKEN=not-this-one\n");

        let parsed = parse_config_disk(&disk).expect("the disk parses");

        assert_eq!(parsed.get("ORB_TOKEN").map(String::as_str), Some("secret"));
    }

    #[test]
    fn a_config_disk_without_a_token_is_refused() {
        let directory = std::env::temp_dir().join("jorb-init-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let disk_path = directory.join("no-token.cfg");
        std::fs::write(&disk_path, config_disk("AMP_URL=https://a.test\n")).unwrap();

        let error = environment(Path::new("/nonexistent-env-file"), &disk_path)
            .expect_err("a config disk without a token is refused");

        assert!(error.to_string().contains("ORB_TOKEN"));
    }

    #[test]
    fn the_config_disk_wins_over_the_image_environment() {
        let directory = std::env::temp_dir().join("jorb-init-tests");
        std::fs::create_dir_all(&directory).unwrap();

        let env_path = directory.join("jorb-env");
        std::fs::write(
            &env_path,
            "AMP_URL=https://image.test\nORB_TOKEN=from-image\n",
        )
        .unwrap();

        let disk_path = directory.join("with-token.cfg");
        std::fs::write(
            &disk_path,
            config_disk("ORB_TOKEN=from-host\nAMP_URL=https://host.test\n"),
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
}
