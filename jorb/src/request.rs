//! What Amp asked for, parsed once from the untrusted `ORB_*` variables. This
//! is the privilege boundary: every refusal jorb makes of Amp's input is here.

use std::fmt;
use std::io;
use std::str::FromStr;

use anyhow::{Result, bail, ensure};
use jorb_config_disk::ConfigDisk;

use crate::limits;
use crate::operations::Operation;
use crate::variables::Variables;

#[derive(Debug)]
pub struct Request {
    pub orb: OrbId,
    pub operation: Operation,
}

impl Request {
    pub fn parse(variables: &Variables) -> Result<Self> {
        let orb = variables.required("ORB_ID")?.parse()?;
        let operation = match variables.required("ORB_OPERATION")? {
            "spawn" => {
                let config_disk = ConfigDisk {
                    orb_token: variables.required("ORB_TOKEN")?.to_owned(),
                    amp_url: variables.optional("AMP_URL").map(str::to_owned),
                };
                ensure!(!config_disk.orb_token.is_empty(), "ORB_TOKEN is empty");
                config_disk.write_to(io::sink())?;
                Operation::Spawn(config_disk)
            }
            "pause" => Operation::Pause,
            "delete" => Operation::Delete,
            other => bail!("ORB_OPERATION is {other:?}, which is not spawn, pause or delete"),
        };
        Ok(Self { orb, operation })
    }
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

    fn parse(variables: &[(&str, &str)]) -> Result<Request> {
        Request::parse(&Variables::of(variables))
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
        let request = parse(&[
            ("ORB_OPERATION", "spawn"),
            ("ORB_ID", "amp-1"),
            ("ORB_TOKEN", "must-not-leak"),
            ("AMP_URL", "https://amp.example"),
        ])
        .unwrap();
        assert!(!format!("{request:?}").contains("must-not-leak"));

        let Operation::Spawn(disk) = request.operation else {
            panic!("a spawn");
        };
        assert_eq!(disk.orb_token, "must-not-leak");
        assert_eq!(disk.amp_url.as_deref(), Some("https://amp.example"));
    }
}
