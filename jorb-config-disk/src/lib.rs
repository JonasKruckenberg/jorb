//! The config disk: the whole interface between jorb on the host and jorb-init
//! in the guest, as one type both compile against.
//!
//! It is JSON padded with spaces to [`BYTES`], so the guest sees one whole
//! block; JSON ignores the trailing whitespace.

use std::fmt;
use std::io::{self, Read, Write};

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
    /// Writes exactly [`BYTES`], or fails before writing past them.
    pub fn write_to(&self, writer: impl Write) -> io::Result<()> {
        let mut block = Block { writer, written: 0 };
        serde_json::to_writer(&mut block, self)?;
        let padding = vec![b' '; BYTES - block.written];
        block.write_all(&padding)
    }

    pub fn read_from(reader: impl Read) -> serde_json::Result<Self> {
        serde_json::from_reader(reader.take(BYTES as u64))
    }
}

/// A writer that refuses to go past one block.
struct Block<W> {
    writer: W,
    written: usize,
}

impl<W: Write> Write for Block<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.written + bytes.len() > BYTES {
            return Err(io::Error::other(format!(
                "the config disk is larger than the {BYTES} bytes it may take"
            )));
        }
        let written = self.writer.write(bytes)?;
        self.written += written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
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
            disk(" a \"quoted\"\nline\0 é ", Some("https://amp.example/?a=b")),
        ] {
            let mut bytes = Vec::new();
            written.write_to(&mut bytes).unwrap();
            assert_eq!(bytes.len(), BYTES);
            assert!(ConfigDisk::read_from(bytes.as_slice()).unwrap() == written);
        }

        let mut bytes = Vec::new();
        let refusal = disk(&"a".repeat(BYTES), None)
            .write_to(&mut bytes)
            .unwrap_err();
        assert!(
            refusal.to_string().contains("larger than the 4096 bytes"),
            "{refusal}"
        );
        assert!(bytes.len() <= BYTES);
    }

    #[test]
    fn the_token_is_not_in_debug_and_unknown_keys_are_refused() {
        assert!(!format!("{:?}", disk("must-not-leak", None)).contains("must-not-leak"));
        assert!(ConfigDisk::read_from(&br#"{"orb_token":"t","extra":1}"#[..]).is_err());
        assert!(ConfigDisk::read_from(&br#"{"amp_url":"x"}"#[..]).is_err());
    }
}
