//! The untrusted edge: what Amp asked for, parsed once into types the rest of
//! jorb can trust.
//!
//! Everything here arrives as an environment variable through one sudo rule,
//! so this module is the privilege boundary. `ORB_*` comes from the provider
//! and is untrusted; `JORB_*` is set by the Nix wrapper on the root side and
//! describes the host.

use std::num::{NonZeroU8, NonZeroU16, NonZeroU32};
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, ensure};
use clap::{Args, Parser, ValueEnum};
use secrecy::SecretString;

use crate::limits;

/// One job, as the provider invokes jorb: an operation, an orb, and on spawn
/// the credential that orb will connect with.
#[derive(Debug, Parser)]
#[command(about = "Runs one Amp orb job on this host")]
pub struct Job {
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

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<OrbId, anyhow::Error> {
        raw.parse()
    }

    /// The reason an id was refused, so a test can say which rule caught it.
    fn refusal(raw: &str) -> String {
        parse(raw)
            .expect_err("this id should be refused")
            .to_string()
    }

    #[test]
    fn an_ordinary_amp_id_is_accepted() {
        assert_eq!(
            parse("amp-T-1a2b3c4d").unwrap().to_string(),
            "amp-T-1a2b3c4d"
        );
        assert_eq!(parse("a").unwrap().to_string(), "a");
        assert_eq!(parse("orb_1").unwrap().to_string(), "orb_1");
    }

    #[test]
    fn sixty_four_characters_fit_and_sixty_five_do_not() {
        let longest = "a".repeat(limits::MAX_ORB_ID_LEN);
        assert_eq!(parse(&longest).unwrap().to_string(), longest);

        let too_long = "a".repeat(limits::MAX_ORB_ID_LEN + 1);
        assert!(refusal(&too_long).contains("longer than 64 characters"));
    }

    #[test]
    fn an_empty_id_is_refused() {
        assert!(refusal("").contains("cannot be empty"));
    }

    #[test]
    fn an_id_that_could_be_a_flag_is_refused() {
        assert!(refusal("-rf").contains("must start with"));
        assert!(refusal("_x").contains("must start with"));
    }

    #[test]
    fn an_id_that_could_be_a_path_is_refused() {
        assert!(refusal("../x").contains("must start with"));
        assert!(refusal(".").contains("must start with"));
        assert!(refusal("/").contains("must start with"));
        assert!(refusal("a.b").contains("may only contain"));
        assert!(refusal("a/b").contains("may only contain"));
    }

    #[test]
    fn an_id_with_whitespace_or_control_characters_is_refused() {
        for raw in ["a b", "a\nb", "a\0b", "a$b", "a\tb"] {
            assert!(
                refusal(raw).contains("may only contain"),
                "{raw:?} should be refused by the character rule"
            );
        }
    }

    #[test]
    fn the_token_does_not_appear_in_a_jobs_debug_output() {
        let job = Job::try_parse_from([
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
        .expect("the job parses");

        let printed = format!("{job:?}");

        assert!(
            !printed.contains("a-token-that-must-not-leak"),
            "the token leaked into Debug: {printed}"
        );
        assert!(printed.contains("amp-1"));
    }

    #[test]
    fn the_settings_describe_the_v1_host_by_default() {
        let job = Job::try_parse_from([
            "jorb",
            "--operation",
            "delete",
            "--orb",
            "amp-1",
            "--kernel",
            "/var/lib/jorb/kernel",
        ])
        .expect("the job parses");

        assert_eq!(job.settings.slots.get(), 1);
        assert_eq!(job.settings.vcpus.get(), 4);
        assert_eq!(job.settings.memory_mib.get(), 6144);
        assert_eq!(job.settings.state_dir, PathBuf::from(limits::STATE_DIR));
        assert_eq!(job.operation, Operation::Delete);
        assert!(job.token.is_none());
    }

    #[test]
    fn an_unknown_operation_is_refused() {
        let job = Job::try_parse_from([
            "jorb",
            "--operation",
            "destroy",
            "--orb",
            "amp-1",
            "--kernel",
            "/k",
        ]);

        assert!(job.is_err());
    }
}
