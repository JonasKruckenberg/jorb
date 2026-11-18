//! The untrusted edge: what Amp asked for, parsed once into types the rest of
//! jorb can trust.
//!
//! Every value here arrives as an `ORB_*` environment variable from the
//! provider, which is closed-source and unprivileged. This module is the
//! privilege boundary: every refusal jorb makes of Amp's input is in it.
//! What the host is, as against what Amp wants, lives in `host.rs`.

use std::str::FromStr;

use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use secrecy::{ExposeSecret, SecretString};

use crate::fixed;

/// What Amp asked for: an operation, the orb to apply it to, and on spawn the
/// credential that orb will connect with.
#[derive(Debug, Args)]
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
}

/// The target state an operation converges on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum Operation {
    Spawn,
    Pause,
    Delete,
}

/// An orb id, checked once to be safe as a file name, a unit name and a
/// hostname.
///
/// Amp sends ids like `amp-<thread id>`. jorb uses them unescaped in all
/// three places, so anything that could mean something to a path, a shell or
/// a resolver is refused here instead of escaped later.
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
            raw.len() <= fixed::MAX_ORB_ID_LEN,
            "an orb id cannot be longer than {} characters, and this one is {}",
            fixed::MAX_ORB_ID_LEN,
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
            contents.len() <= fixed::CONFIG_DISK_BYTES,
            "the config disk needs {} bytes, which is more than the {} it has",
            contents.len(),
            fixed::CONFIG_DISK_BYTES
        );

        let mut disk = contents.into_bytes();
        disk.resize(fixed::CONFIG_DISK_BYTES, 0);

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
    use clap::Parser;

    use super::*;

    /// `Request` is flattened into the real parser, so a test needs one of its
    /// own to parse it alone.
    #[derive(Parser)]
    struct Only {
        #[command(flatten)]
        request: Request,
    }

    fn parse(arguments: &[&str]) -> Request {
        Only::try_parse_from(arguments)
            .expect("the request parses")
            .request
    }

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
            &"a".repeat(fixed::MAX_ORB_ID_LEN),
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
                &"a".repeat(fixed::MAX_ORB_ID_LEN + 1),
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
    fn an_unknown_operation_is_refused() {
        assert!(
            Only::try_parse_from(["jorb", "--operation", "destroy", "--orb", "amp-1"]).is_err()
        );
    }

    #[test]
    fn the_token_does_not_appear_in_a_requests_debug_output() {
        let request = parse(&[
            "jorb",
            "--operation",
            "spawn",
            "--orb",
            "amp-1",
            "--token",
            "a-token-that-must-not-leak",
        ]);

        let printed = format!("{request:?}");

        assert!(
            !printed.contains("a-token-that-must-not-leak"),
            "the token leaked into Debug: {printed}"
        );
        assert!(printed.contains("amp-1"));
    }

    #[test]
    fn a_request_without_a_token_parses_because_only_spawn_needs_one() {
        let request = parse(&["jorb", "--operation", "delete", "--orb", "amp-1"]);

        assert_eq!(request.operation, Operation::Delete);
        assert!(request.token.is_none());
    }

    #[test]
    fn a_config_disk_is_its_lines_padded_to_a_fixed_size() {
        let disk = config_disk("a-token", None).expect("the config disk is representable");

        assert_eq!(disk.as_bytes().len(), fixed::CONFIG_DISK_BYTES);
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
        let refusal = config_disk(&"a".repeat(fixed::CONFIG_DISK_BYTES), None)
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
