//! The untrusted edge: what Amp asked for, parsed once into types the rest of
//! jorb can trust.
//!
//! Everything here arrives as an environment variable through one sudo rule,
//! so this module is the privilege boundary and every refusal jorb makes of
//! Amp's input is in it. `ORB_*` comes from the provider and is untrusted;
//! `JORB_*` is set by the Nix wrapper on the root side and describes the host.

use std::num::{NonZeroU8, NonZeroU16, NonZeroU32};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, ValueEnum};
use secrecy::{ExposeSecret, SecretString};

use crate::limits;

/// One job as the provider asked for it: an operation, an orb, and on spawn
/// the credential that orb will connect with.
#[derive(Debug, Parser)]
#[command(about = "Runs one Amp orb job on this host")]
pub struct Request {
    #[arg(long, env = "ORB_OPERATION")]
    pub operation: Operation,

    #[arg(long, env = "ORB_ID")]
    pub orb: OrbId,

    /// Present on spawn only, and short-lived: Amp mints it per spawn job.
    #[arg(long, env = "ORB_TOKEN")]
    pub token: Option<SecretString>,

    /// A non-default Amp endpoint, when the provider was told to use one.
    #[arg(long, env = "AMP_URL")]
    pub amp_url: Option<String>,

    #[command(flatten)]
    pub settings: Settings,
}

/// The target state an operation converges on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum Operation {
    Spawn,
    Pause,
    Delete,
}

/// What this host is, as the Nix module describes it. Never from the caller.
#[derive(Debug, Args)]
pub struct Settings {
    /// How many orbs may run at once. Each one holds a slot.
    #[arg(long, env = "JORB_SLOTS", default_value = "1")]
    pub slots: NonZeroU16,

    #[arg(long, env = "JORB_VCPUS", default_value = "4")]
    pub vcpus: NonZeroU8,

    #[arg(long, env = "JORB_MEMORY_MIB", default_value = "6144")]
    pub memory_mib: NonZeroU32,

    /// The guest kernel, which `nix build .#guest-kernel` produces.
    #[arg(long, env = "JORB_KERNEL")]
    pub kernel: PathBuf,

    /// Where the base image and the orbs' disks live. XFS, for reflinks.
    #[arg(long, env = "JORB_STATE_DIR", default_value = limits::STATE_DIR)]
    pub state_dir: PathBuf,

    /// Where the config disks and API sockets live. Cleared on boot.
    #[arg(long, env = "JORB_RUN_DIR", default_value = limits::RUN_DIR)]
    pub run_dir: PathBuf,

    /// The VMM to run. A setting so the bounds tests can point it at a
    /// program that never opens its socket.
    #[arg(long, env = "JORB_CH_BIN", default_value = limits::CLOUD_HYPERVISOR)]
    pub cloud_hypervisor: PathBuf,
}

/// An orb id, checked once to be safe as a file name, a unit name and a
/// hostname.
///
/// Amp sends ids like `amp-<thread id>`. jorb uses them unescaped in all
/// three places, which is why anything that could mean something to a path, a
/// shell or a resolver is refused here rather than escaped later.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrbId(String);

impl FromStr for OrbId {
    // anyhow rather than an error enum: this is a binary, so the message is
    // the whole interface. The refused id is safe to print, having come from
    // the provider rather than from a secret.
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        let mut characters = raw.chars();

        let first = characters.next().context("an orb id cannot be empty")?;
        ensure!(
            first.is_ascii_alphanumeric(),
            "an orb id must start with a letter or a digit, and {first:?} does not"
        );
        ensure!(
            raw.len() <= limits::MAX_ORB_ID_LEN,
            "an orb id cannot be longer than {} characters, and this one is {}",
            limits::MAX_ORB_ID_LEN,
            raw.len()
        );

        for found in characters {
            ensure!(
                found.is_ascii_alphanumeric() || found == '-' || found == '_',
                "an orb id may only contain letters, digits, - and _, and {found:?} is none of those"
            );
        }

        Ok(Self(raw.to_owned()))
    }
}

impl std::fmt::Display for OrbId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The config disk: the only thing the host tells one particular orb.
///
/// It holds the block the guest will read rather than the values it was built
/// from, so there is one representation of a config rather than two, and
/// "already running with this config" is a byte comparison against the file on
/// the host.
pub struct ConfigDisk(Vec<u8>);

impl ConfigDisk {
    /// `KEY=VALUE` lines, NUL-padded to the disk's fixed size.
    ///
    /// A value carrying a newline or a NUL is refused: the first would make
    /// one value look like two, the second would end the disk where the
    /// padding starts. The offending value never reaches the message, because
    /// one of the two is the token.
    pub fn new(token: Option<&SecretString>, amp_url: Option<&str>) -> Result<Self> {
        let token = token.context("spawn needs ORB_TOKEN, and this job has none")?;

        let mut contents = String::new();
        for (key, value) in [
            ("ORB_TOKEN", Some(token.expose_secret())),
            ("AMP_URL", amp_url),
        ] {
            let Some(value) = value else { continue };
            ensure!(
                !value.contains(['\n', '\0']),
                "the value of {key} contains a newline or a NUL, which a config disk cannot carry"
            );
            contents.push_str(&format!("{key}={value}\n"));
        }

        ensure!(
            contents.len() <= limits::CONFIG_DISK_BYTES,
            "the config disk needs {} bytes, which is more than the {} it has",
            contents.len(),
            limits::CONFIG_DISK_BYTES
        );

        let mut disk = contents.into_bytes();
        disk.resize(limits::CONFIG_DISK_BYTES, 0);

        Ok(Self(disk))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for ConfigDisk {
    /// Never the contents: one of the values on the disk is the token.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ConfigDisk({} bytes)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(raw: &str) -> String {
        raw.parse::<OrbId>()
            .expect_err("this id should be refused")
            .to_string()
    }

    fn config_disk(token: &str, amp_url: Option<&str>) -> Result<ConfigDisk> {
        ConfigDisk::new(Some(&token.into()), amp_url)
    }

    #[test]
    fn an_ordinary_amp_id_is_accepted() {
        for raw in [
            "amp-T-1a2b3c4d",
            "a",
            "orb_1",
            &"a".repeat(limits::MAX_ORB_ID_LEN),
        ] {
            assert_eq!(
                raw.parse::<OrbId>().expect("this id is valid").to_string(),
                raw
            );
        }
    }

    /// Each of these would mean something somewhere jorb puts an id unescaped:
    /// a flag, a path, a shell word or a hostname.
    #[test]
    fn an_id_that_could_mean_something_elsewhere_is_refused() {
        for (raw, reason) in [
            ("", "cannot be empty"),
            (
                &"a".repeat(limits::MAX_ORB_ID_LEN + 1),
                "longer than 64 characters",
            ),
            ("-rf", "must start with"),
            ("_x", "must start with"),
            ("../x", "must start with"),
            (".", "must start with"),
            ("/", "must start with"),
            ("a.b", "may only contain"),
            ("a/b", "may only contain"),
            ("a b", "may only contain"),
            ("a\nb", "may only contain"),
            ("a\0b", "may only contain"),
            ("a$b", "may only contain"),
            ("a\tb", "may only contain"),
        ] {
            let refusal = refusal(raw);

            assert!(
                refusal.contains(reason),
                "{raw:?} was refused with {refusal:?}, which does not mention {reason:?}"
            );
        }
    }

    #[test]
    fn the_token_does_not_appear_in_a_jobs_debug_output() {
        let request = Request::try_parse_from([
            "jorb",
            "--operation",
            "spawn",
            "--orb",
            "amp-1",
            "--token",
            "a-token-that-must-not-leak",
            "--kernel",
            "/var/lib/jorb/kernel",
        ])
        .expect("the request parses");

        let printed = format!("{request:?}");

        assert!(
            !printed.contains("a-token-that-must-not-leak"),
            "the token leaked into Debug: {printed}"
        );
        assert!(printed.contains("amp-1"));
    }

    #[test]
    fn the_settings_describe_the_v1_host_by_default() {
        let request = Request::try_parse_from([
            "jorb",
            "--operation",
            "delete",
            "--orb",
            "amp-1",
            "--kernel",
            "/var/lib/jorb/kernel",
        ])
        .expect("the request parses");

        assert_eq!(request.settings.slots.get(), 1);
        assert_eq!(request.settings.vcpus.get(), 4);
        assert_eq!(request.settings.memory_mib.get(), 6144);
        assert_eq!(request.settings.state_dir, PathBuf::from(limits::STATE_DIR));
        assert_eq!(request.operation, Operation::Delete);
        assert!(request.token.is_none());
    }

    #[test]
    fn an_unknown_operation_is_refused() {
        assert!(
            Request::try_parse_from([
                "jorb",
                "--operation",
                "destroy",
                "--orb",
                "amp-1",
                "--kernel",
                "/k",
            ])
            .is_err()
        );
    }

    #[test]
    fn a_config_disk_is_its_lines_padded_to_a_fixed_size() {
        let disk = config_disk("a-token", None).expect("the config disk is representable");

        assert_eq!(disk.as_bytes().len(), limits::CONFIG_DISK_BYTES);
        assert_eq!(&disk.as_bytes()[..18], b"ORB_TOKEN=a-token\n");
        assert!(disk.as_bytes()[18..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn the_amp_url_is_only_there_when_it_was_given() {
        let without = config_disk("a-token", None).unwrap();
        let with = config_disk("a-token", Some("https://amp.example")).unwrap();

        assert!(!String::from_utf8_lossy(without.as_bytes()).contains("AMP_URL"));
        assert!(String::from_utf8_lossy(with.as_bytes()).contains("AMP_URL=https://amp.example\n"));
    }

    #[test]
    fn spawn_without_a_token_is_refused() {
        let refusal = ConfigDisk::new(None, None)
            .expect_err("a spawn without a token has nothing to write")
            .to_string();

        assert!(refusal.contains("ORB_TOKEN"), "{refusal}");
    }

    #[test]
    fn a_value_the_disk_could_not_carry_back_out_is_refused() {
        for token in ["two\nlines", "a\0b", "trailing\n"] {
            let refusal = config_disk(token, None)
                .expect_err("this token is not representable")
                .to_string();

            assert!(refusal.contains("ORB_TOKEN"), "{token:?}: {refusal}");
            assert!(!refusal.contains(token), "the token leaked: {refusal}");
        }

        let refusal = config_disk("a-token", Some("https://a\nb"))
            .expect_err("this url is not representable")
            .to_string();

        assert!(refusal.contains("AMP_URL"), "{refusal}");
    }

    #[test]
    fn a_config_that_does_not_fit_is_refused() {
        let refusal = config_disk(&"a".repeat(limits::CONFIG_DISK_BYTES), None)
            .expect_err("a config that does not fit is refused")
            .to_string();

        assert!(refusal.contains("more than the 4096 it has"), "{refusal}");
    }

    #[test]
    fn the_token_does_not_appear_in_a_config_disks_debug_output() {
        let disk = config_disk("a-token-that-must-not-leak", None).unwrap();

        let printed = format!("{disk:?}");

        assert!(
            !printed.contains("a-token-that-must-not-leak"),
            "the token leaked into Debug: {printed}"
        );
    }
}
