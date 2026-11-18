//! The config disk: the whole interface between jorb on the host and jorb-init
//! in the guest, as one type both compile against.
//!
//! It is JSON, padded with spaces to [`BYTES`] so it is one whole block, and
//! so "identical config" is a byte comparison: serde writes the fields in
//! declaration order, and JSON ignores trailing whitespace.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const BYTES: usize = 4096;

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigDisk {
    /// The orb's credential: never in `Debug`, never logged.
    pub orb_token: String,
    /// A non-default Amp endpoint, when the provider has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amp_url: Option<String>,
}

impl ConfigDisk {
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut disk = serde_json::to_vec(self)?;
        if disk.len() > BYTES {
            return Err(serde::ser::Error::custom(format!(
                "the config disk needs {} of {BYTES} bytes",
                disk.len()
            )));
        }
        disk.resize(BYTES, b' ');
        Ok(disk)
    }

    pub fn decode(disk: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(disk)
    }
}

impl fmt::Debug for ConfigDisk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigDisk")
            .field("amp_url", &self.amp_url)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(token: &str, amp_url: Option<&str>) -> ConfigDisk {
        ConfigDisk {
            orb_token: token.into(),
            amp_url: amp_url.map(Into::into),
        }
    }

    #[test]
    fn a_disk_is_one_block_and_reads_back_as_written() {
        for written in [
            disk("t", None),
            disk(
                " a \"quoted\"\nline\0 with é ",
                Some("https://amp.example/?a=b"),
            ),
        ] {
            let bytes = written.encode().unwrap();
            assert_eq!(bytes.len(), BYTES);
            assert!(ConfigDisk::decode(&bytes).unwrap() == written);
        }
        assert!(disk(&"a".repeat(BYTES), None).encode().is_err());
    }

    #[test]
    fn the_token_is_not_in_debug_and_unknown_keys_are_refused() {
        assert!(!format!("{:?}", disk("must-not-leak", None)).contains("must-not-leak"));
        assert!(ConfigDisk::decode(br#"{"orb_token":"t","extra":1}"#).is_err());
        assert!(ConfigDisk::decode(br#"{"amp_url":"x"}"#).is_err());
    }
}
