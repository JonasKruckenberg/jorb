//! Guest PID 1: mount, settle the writeback window, resolve DNS, build the
//! environment, hand over to `amp orb bootstrap`, then reap for the life of
//! the guest.
//!
//! It knows nothing about Amp's job protocol and nothing about the host. The
//! config disk on `/dev/vdb` is the whole interface between the two: jorb
//! writes it, this reads it, and neither needs to agree on anything else.
//!
//! Every failure before bootstrap starts ends in a power off. A guest that
//! cannot run bootstrap has nothing to offer, and a stopped unit is the
//! truthful state for the host to see. There is no shutdown path: pause is a
//! power cut, and ext4 replays its journal on the next boot.

mod fixed;

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
    shorten_the_writeback_window()?;
    write_resolv_conf()?;

    let environment = environment(
        Path::new(fixed::ENV_FILE),
        Path::new(fixed::CONFIG_DISK_DEVICE),
    )?;

    spawn_bootstrap(&environment)
}

/// The kernel mounts devtmpfs itself; these are the rest of what a userspace
/// expects to find.
fn mount_filesystems() -> Result<()> {
    for (source, target, filesystem, options) in [
        ("proc", "/proc", "proc", None),
        ("sysfs", "/sys", "sysfs", None),
        ("devpts", "/dev/pts", "devpts", None),
        ("tmpfs", "/dev/shm", "tmpfs", Some(fixed::DEV_SHM_OPTIONS)),
        ("tmpfs", "/run", "tmpfs", None),
    ] {
        fs::create_dir_all(target).with_context(|| format!("creating {target}"))?;

        mount(
            source,
            target,
            filesystem,
            MountFlags::NOSUID | MountFlags::NODEV,
            options,
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
        fs::write(knob, fixed::DIRTY_WRITEBACK_CENTISECS.to_string())
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
///
/// A missing env file is a failure. jorb-prepare-image always writes one, so
/// its absence means this is not the image jorb built, and booting on without
/// the image's PATH would only fail further in.
fn environment(env_file: &Path, config_disk: &Path) -> Result<BTreeMap<String, String>> {
    let image =
        fs::read_to_string(env_file).with_context(|| format!("reading {}", env_file.display()))?;
    let mut environment = parse_env_file(&image);

    environment.insert("HOME".to_owned(), fixed::GUEST_HOME.to_owned());
    environment.insert("USER".to_owned(), fixed::GUEST_USER.to_owned());

    let disk =
        fs::read(config_disk).with_context(|| format!("reading {}", config_disk.display()))?;
    environment.extend(parse_config_disk(&disk)?);

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

/// The config disk: `KEY=VALUE` lines in the first block, NUL-padded to the
/// end. Anything after the padding is not ours to read.
fn parse_config_disk(disk: &[u8]) -> Result<BTreeMap<String, String>> {
    let end = disk
        .iter()
        .take(fixed::CONFIG_DISK_BYTES)
        .position(|byte| *byte == 0)
        .unwrap_or(disk.len().min(fixed::CONFIG_DISK_BYTES));

    let contents = std::str::from_utf8(&disk[..end]).context("the config disk is not UTF-8")?;

    Ok(parse_env_file(contents))
}

/// Hands over to bootstrap as the image's unprivileged user, in its home.
///
/// The child keeps running after this returns: it is the guest's whole
/// purpose, and PID 1 only reaps from here on.
fn spawn_bootstrap(environment: &BTreeMap<String, String>) -> Result<u32> {
    let child = Command::new(fixed::SETPRIV)
        .args([
            &format!("--reuid={}", fixed::GUEST_USER),
            &format!("--regid={}", fixed::GUEST_USER),
            "--init-groups",
            "--",
            fixed::AMP,
            "orb",
            "bootstrap",
        ])
        .current_dir(fixed::GUEST_HOME)
        .env_clear()
        .envs(environment)
        .spawn()
        .with_context(|| format!("spawning {} through {}", fixed::AMP, fixed::SETPRIV))?;

    info!(
        pid = child.id(),
        user = fixed::GUEST_USER,
        "bootstrap started"
    );

    Ok(child.id())
}

/// Reaps children for the life of the guest.
///
/// The one unbounded loop in jorb, and the reason this function returns `!`:
/// PID 1 inherits every orphan in the guest, and a guest ends by being
/// switched off, not by this returning. Bootstrap exiting is logged and
/// nothing more; the orb stays up until the host stops it, as `pause` does.
fn reap(bootstrap: u32) -> ! {
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

/// Switches the guest off. The host sees the unit stop, the truthful state
/// for a guest with nothing left to do.
fn power_off() -> ! {
    if let Err(error) = reboot(RebootCommand::PowerOff) {
        error!(error = error.to_string(), "powering off failed");
    }

    // The syscall does not return when it works, so reaching here means it
    // failed. PID 1 exiting panics the kernel, which stops the VM and so the
    // unit: what the host was going to see anyway. Looping here would be a
    // second unbounded loop, and the reap loop is the only one jorb allows
    // itself.
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

    /// A config disk as jorb writes it: lines, then NUL padding to the block.
    fn config_disk(contents: &str) -> Vec<u8> {
        let mut disk = contents.as_bytes().to_vec();
        disk.resize(fixed::CONFIG_DISK_BYTES, 0);
        disk
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
    fn the_config_disk_ends_where_its_padding_starts() {
        let parsed = parse_config_disk(&config_disk(
            "\nORB_TOKEN=secret\n\nAMP_URL=https://a.test\n",
        ))
        .expect("the disk parses");

        assert_eq!(parsed.get("ORB_TOKEN").map(String::as_str), Some("secret"));
        assert_eq!(
            parsed.get("AMP_URL").map(String::as_str),
            Some("https://a.test")
        );
        assert_eq!(parsed.len(), 2);

        assert!(
            parse_config_disk(&config_disk(""))
                .expect("an empty disk parses")
                .is_empty()
        );

        let mut past_the_block = config_disk("ORB_TOKEN=secret\n");
        past_the_block.extend_from_slice(b"ORB_TOKEN=not-this-one\n");
        assert_eq!(
            parse_config_disk(&past_the_block)
                .expect("the disk parses")
                .get("ORB_TOKEN")
                .map(String::as_str),
            Some("secret")
        );
    }

    #[test]
    fn a_config_disk_without_a_token_is_refused() {
        let directory = test_directory("no-token");
        let env_path = directory.join("jorb-env");
        let disk_path = directory.join("no-token.cfg");
        fs::write(&env_path, "PATH=/usr/bin\n").unwrap();
        fs::write(&disk_path, config_disk("AMP_URL=https://a.test\n")).unwrap();

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

    /// A missing env file means the image is not the one jorb-prepare-image
    /// made, which jorb-init refuses instead of booting on without a PATH.
    #[test]
    fn a_missing_env_file_is_refused() {
        let directory = test_directory("no-env-file");
        let disk_path = directory.join("with-token.cfg");
        fs::write(&disk_path, config_disk("ORB_TOKEN=secret\n")).unwrap();

        let error = environment(&directory.join("not-here"), &disk_path)
            .expect_err("a missing env file is refused");

        assert!(format!("{error:#}").contains("not-here"));
    }
}
