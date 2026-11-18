//! What Amp asked for, parsed once from untrusted `ORB_*` variables. This is
//! the privilege boundary: every refusal jorb makes of Amp's input is here.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use jorb_config_disk::ConfigDisk;
use secrecy::{ExposeSecret, SecretString};

use crate::{Environment, limits, required};

#[derive(Debug)]
pub struct Job {
    pub orb: OrbId,
    pub operation: Operation,
}

/// Only a spawn carries a config disk, so a spawn without a token cannot be
/// represented.
#[derive(Debug)]
pub enum Operation {
    Spawn(ConfigDisk),
    Pause,
    Delete,
}

impl Job {
    pub fn from_environment(environment: Environment) -> Result<Self> {
        let orb = required(environment, "ORB_ID")?.parse()?;
        let operation = match required(environment, "ORB_OPERATION")?.as_str() {
            "spawn" => {
                let token = environment("ORB_TOKEN")?
                    .map(SecretString::from)
                    .context("spawn needs ORB_TOKEN, and this job has none")?;
                Operation::Spawn(config_disk(&token, environment("AMP_URL")?.as_deref())?)
            }
            "pause" => Operation::Pause,
            "delete" => Operation::Delete,
            other => bail!("ORB_OPERATION is {other:?}, which is not spawn, pause or delete"),
        };
        Ok(Self { orb, operation })
    }
}

impl Operation {
    /// As Amp spells it, for the `op` log field.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Spawn(_) => "spawn",
            Self::Pause => "pause",
            Self::Delete => "delete",
        }
    }
}

/// The token and, when the provider has one, the Amp endpoint.
pub fn config_disk(token: &SecretString, amp_url: Option<&str>) -> Result<ConfigDisk> {
    let token = token.expose_secret();
    ensure!(!token.is_empty(), "ORB_TOKEN is empty");

    let mut entries = vec![("ORB_TOKEN", token)];
    entries.extend(amp_url.map(|url| ("AMP_URL", url)));
    Ok(ConfigDisk::encode(&entries)?)
}

/// Safe unescaped as a file name, a unit name and a hostname.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbId(String);

impl FromStr for OrbId {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        ensure!(
            raw.len() <= limits::MAX_ORB_ID_LEN
                && raw.starts_with(|first: char| first.is_ascii_alphanumeric())
                && raw
                    .chars()
                    .all(|found| found.is_ascii_alphanumeric() || found == '-' || found == '_'),
            "{raw:?} is not an orb id: up to {} letters, digits, - and _, starting with a letter or digit",
            limits::MAX_ORB_ID_LEN
        );
        Ok(Self(raw.to_owned()))
    }
}

impl fmt::Display for OrbId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_environment;

    fn parse(variables: &[(&str, &str)]) -> Result<Job> {
        Job::from_environment(&test_environment(variables))
    }

    #[test]
    fn ids_are_safe_as_paths_units_and_hostnames() {
        let longest = "a".repeat(limits::MAX_ORB_ID_LEN);
        for valid in ["amp-T-1a2b3c4d", "a", "orb_1", &longest] {
            assert_eq!(valid.parse::<OrbId>().unwrap().to_string(), valid);
        }

        let too_long = "a".repeat(limits::MAX_ORB_ID_LEN + 1);
        for invalid in [
            "", &too_long, "-rf", "_x", "../x", ".", "/", "a.b", "a/b", "a b", "a\nb", "a\0b",
            "a$b",
        ] {
            assert!(invalid.parse::<OrbId>().is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn only_a_spawn_needs_a_token_and_it_must_not_be_empty() {
        let pause = parse(&[("ORB_OPERATION", "pause"), ("ORB_ID", "amp-1")]).unwrap();
        assert_eq!(pause.operation.name(), "pause");

        for token in [None, Some("")] {
            let mut variables = vec![("ORB_OPERATION", "spawn"), ("ORB_ID", "amp-1")];
            variables.extend(token.map(|token| ("ORB_TOKEN", token)));
            let refusal = format!("{:#}", parse(&variables).unwrap_err());
            assert!(refusal.contains("ORB_TOKEN"), "{refusal}");
        }

        for operation in ["destroy", "Spawn"] {
            assert!(parse(&[("ORB_OPERATION", operation), ("ORB_ID", "amp-1")]).is_err());
        }
    }

    #[test]
    fn the_token_reaches_the_config_disk_and_nowhere_else() {
        let job = parse(&[
            ("ORB_OPERATION", "spawn"),
            ("ORB_ID", "amp-1"),
            ("ORB_TOKEN", "must-not-leak"),
            ("AMP_URL", "https://amp.example"),
        ])
        .unwrap();
        assert!(!format!("{job:?}").contains("must-not-leak"));

        let Operation::Spawn(disk) = job.operation else {
            panic!("a spawn");
        };
        assert!(
            disk.as_bytes()
                .starts_with(b"ORB_TOKEN=must-not-leak\nAMP_URL=https://amp.example\n")
        );

        let refusal = config_disk(&"two\nlines".into(), None)
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("ORB_TOKEN") && !refusal.contains("two"),
            "{refusal}"
        );
    }
}
