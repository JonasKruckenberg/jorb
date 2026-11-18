//! The untrusted edge: what Amp asked for, parsed once into types the rest of
//! jorb can trust.
//!
//! Every value here arrives as an `ORB_*` environment variable from the
//! provider, which is closed-source and unprivileged. This module is the
//! privilege boundary: every refusal jorb makes of Amp's input is in it.
//! What the host is, as against what Amp wants, lives in `host.rs`.

use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use jorb_config_disk::ConfigDisk;
use secrecy::{ExposeSecret, SecretString};

use crate::{Environment, constants, required};

/// What Amp asked for: an operation on one orb, with what that operation
/// needs and nothing else. Only a spawn carries a config disk, so a spawn
/// without a token cannot be represented.
#[derive(Debug)]
pub enum Job {
    Spawn { orb: OrbId, config_disk: ConfigDisk },
    Pause { orb: OrbId },
    Delete { orb: OrbId },
}

impl Job {
    pub fn from_environment(environment: Environment) -> Result<Self> {
        let operation = required(environment, "ORB_OPERATION")?;
        let orb = required(environment, "ORB_ID")?.parse().context("ORB_ID")?;

        match operation.as_str() {
            "spawn" => {
                // Amp mints the token per spawn job.
                let token = environment("ORB_TOKEN")?
                    .map(SecretString::from)
                    .context("spawn needs ORB_TOKEN, and this job has none")?;
                let config_disk = config_disk(&token, environment("AMP_URL")?.as_deref())?;
                Ok(Self::Spawn { orb, config_disk })
            }
            "pause" => Ok(Self::Pause { orb }),
            "delete" => Ok(Self::Delete { orb }),
            _ => bail!("ORB_OPERATION is {operation:?}, which is not spawn, pause or delete"),
        }
    }

    /// The operation's name as Amp spells it, for the `op` log field.
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Spawn { .. } => "spawn",
            Self::Pause { .. } => "pause",
            Self::Delete { .. } => "delete",
        }
    }

    pub fn orb(&self) -> &OrbId {
        match self {
            Self::Spawn { orb, .. } | Self::Pause { orb } | Self::Delete { orb } => orb,
        }
    }
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
            raw.len() <= constants::MAX_ORB_ID_LEN,
            "an orb id cannot be longer than {} characters, and this one is {}",
            constants::MAX_ORB_ID_LEN,
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

/// The config disk for a spawn: the token, and the Amp endpoint when the
/// provider was given one. The token is refused when empty, because the orb
/// would have nothing to connect with.
pub fn config_disk(token: &SecretString, amp_url: Option<&str>) -> Result<ConfigDisk> {
    let token = token.expose_secret();
    ensure!(
        !token.is_empty(),
        "ORB_TOKEN is empty, so the orb would have nothing to connect with"
    );

    let mut entries = vec![("ORB_TOKEN", token)];
    if let Some(amp_url) = amp_url {
        entries.push(("AMP_URL", amp_url));
    }

    Ok(ConfigDisk::encode(&entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_environment;

    fn parse(variables: &[(&str, &str)]) -> Result<Job> {
        Job::from_environment(&test_environment(variables))
    }

    fn refusal(raw: &str) -> String {
        raw.parse::<OrbId>()
            .expect_err("this id should be refused")
            .to_string()
    }

    fn encoded(token: &str, amp_url: Option<&str>) -> Result<ConfigDisk> {
        config_disk(&token.into(), amp_url)
    }

    #[test]
    fn an_ordinary_amp_id_is_accepted() {
        for raw in [
            "amp-T-1a2b3c4d",
            "a",
            "orb_1",
            &"a".repeat(constants::MAX_ORB_ID_LEN),
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
                &"a".repeat(constants::MAX_ORB_ID_LEN + 1),
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
        assert!(parse(&[("ORB_OPERATION", "destroy"), ("ORB_ID", "amp-1")]).is_err());
        assert!(parse(&[("ORB_OPERATION", "Spawn"), ("ORB_ID", "amp-1")]).is_err());
        assert!(parse(&[("ORB_ID", "amp-1")]).is_err());
    }

    #[test]
    fn the_token_does_not_appear_in_a_jobs_debug_output() {
        let job = parse(&[
            ("ORB_OPERATION", "spawn"),
            ("ORB_ID", "amp-1"),
            ("ORB_TOKEN", "a-token-that-must-not-leak"),
        ])
        .unwrap();

        let printed = format!("{job:?}");

        assert!(
            !printed.contains("a-token-that-must-not-leak"),
            "the token leaked into Debug: {printed}"
        );
        assert!(printed.contains("amp-1"));
    }

    #[test]
    fn only_spawn_needs_a_token() {
        for operation in ["pause", "delete"] {
            let job = parse(&[("ORB_OPERATION", operation), ("ORB_ID", "amp-1")]).unwrap();
            assert_eq!(job.operation(), operation);
            assert_eq!(job.orb().to_string(), "amp-1");
        }
    }

    #[test]
    fn spawn_without_a_token_is_a_parse_error() {
        for variables in [
            &[("ORB_OPERATION", "spawn"), ("ORB_ID", "amp-1")][..],
            &[
                ("ORB_OPERATION", "spawn"),
                ("ORB_ID", "amp-1"),
                ("ORB_TOKEN", ""),
            ][..],
        ] {
            let refusal = format!("{:#}", parse(variables).expect_err("refused"));
            assert!(refusal.contains("ORB_TOKEN"), "{refusal}");
        }
    }

    #[test]
    fn the_amp_url_is_only_there_when_it_was_given() {
        let without = encoded("a-token", None).unwrap();
        let with = encoded("a-token", Some("https://amp.example")).unwrap();

        assert!(!String::from_utf8_lossy(without.as_bytes()).contains("AMP_URL"));
        assert!(String::from_utf8_lossy(with.as_bytes()).contains("AMP_URL=https://amp.example\n"));
    }

    #[test]
    fn a_value_the_disk_could_not_carry_back_out_is_refused() {
        for token in ["two\nlines", "a\0b", "trailing\n"] {
            let refusal = encoded(token, None)
                .expect_err("this token is not representable")
                .to_string();

            assert!(refusal.contains("ORB_TOKEN"), "{token:?}: {refusal}");
            assert!(!refusal.contains(token), "the token leaked: {refusal}");
        }

        let refusal = encoded("a-token", Some("https://a\nb"))
            .expect_err("this url is not representable")
            .to_string();

        assert!(refusal.contains("AMP_URL"), "{refusal}");
    }
}
