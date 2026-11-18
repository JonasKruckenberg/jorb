//! The config disk: the whole interface between jorb on the host and
//! jorb-init in the guest, defined once so the two cannot disagree.
//!
//! `KEY=VALUE\n` lines in UTF-8, NUL-padded to [`BYTES`]. Nothing is trimmed
//! or interpreted: a value reads back exactly as it was written, so a key or
//! value that could not survive that trip is refused when encoding. The crate
//! knows nothing about which keys exist; jorb and jorb-init decide that.
//!
//! No error message carries a value, because one of the values is a token.

use std::fmt;

/// The config disk's size. One block holds the keys v1 has many times over,
/// and a fixed size makes "identical config" a byte comparison.
pub const BYTES: usize = 4096;

/// An encoded config disk. Its `Debug` never shows the contents.
#[derive(PartialEq, Eq)]
pub struct ConfigDisk(Vec<u8>);

/// Why a config disk could not be encoded or read.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// A key was empty or held `=`, a newline or a NUL.
    Key { key: String },
    /// The value of this key held a newline or a NUL.
    Value { key: String },
    /// The lines need this many bytes, more than [`BYTES`].
    TooLarge { needed: usize },
    /// The disk's contents are not UTF-8.
    NotUtf8,
    /// Line `line` (from 1) has no `=`.
    NotAnAssignment { line: usize },
}

impl ConfigDisk {
    /// One line per entry, in the order given.
    pub fn encode(entries: &[(&str, &str)]) -> Result<Self, Error> {
        let mut contents = String::new();

        for (key, value) in entries {
            if key.is_empty() || key.contains(['=', '\n', '\0']) {
                return Err(Error::Key {
                    key: (*key).to_owned(),
                });
            }
            if value.contains(['\n', '\0']) {
                return Err(Error::Value {
                    key: (*key).to_owned(),
                });
            }
            contents.push_str(key);
            contents.push('=');
            contents.push_str(value);
            contents.push('\n');
        }

        if contents.len() > BYTES {
            return Err(Error::TooLarge {
                needed: contents.len(),
            });
        }

        let mut disk = contents.into_bytes();
        disk.resize(BYTES, 0);
        Ok(Self(disk))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// The entries on a disk, in order. Only the first [`BYTES`] are read, and
/// they end at the first NUL. Empty lines are skipped.
pub fn parse(disk: &[u8]) -> Result<Vec<(String, String)>, Error> {
    let block = &disk[..disk.len().min(BYTES)];
    let end = block
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(block.len());
    let contents = std::str::from_utf8(&block[..end]).map_err(|_| Error::NotUtf8)?;

    let mut entries = Vec::new();
    for (index, line) in contents.split('\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(Error::NotAnAssignment { line: index + 1 })?;
        entries.push((key.to_owned(), value.to_owned()));
    }

    Ok(entries)
}

impl fmt::Debug for ConfigDisk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ConfigDisk({} bytes)", self.0.len())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key { key } => write!(
                formatter,
                "the key {key:?} is empty or contains =, a newline or a NUL"
            ),
            Self::Value { key } => write!(
                formatter,
                "the value of {key} contains a newline or a NUL, which a config disk cannot carry"
            ),
            Self::TooLarge { needed } => write!(
                formatter,
                "the config disk needs {needed} bytes, which is more than the {BYTES} it has"
            ),
            Self::NotUtf8 => write!(formatter, "the config disk is not UTF-8"),
            Self::NotAnAssignment { line } => {
                write!(formatter, "line {line} of the config disk has no =")
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Characters chosen to break a parser that trims, splits on the wrong
    /// `=`, treats `\r` as a line end, or assumes ASCII.
    const ALPHABET: [&str; 8] = ["a", " ", "=", "\r", "\t", "#", "é", "\u{1F600}"];
    const LONGEST: u32 = 3;

    /// Every string of up to three characters from [`ALPHABET`], the empty
    /// one included.
    fn values() -> Vec<String> {
        let mut values = Vec::new();
        for length in 0..=LONGEST {
            for index in 0..ALPHABET.len().pow(length) {
                let mut value = String::new();
                let mut rest = index;
                for _ in 0..length {
                    value.push_str(ALPHABET[rest % ALPHABET.len()]);
                    rest /= ALPHABET.len();
                }
                values.push(value);
            }
        }
        values
    }

    #[test]
    fn every_value_the_encoder_accepts_parses_back_byte_identical() {
        let values = values();
        assert_eq!(values.len(), 1 + 8 + 64 + 512);

        for value in &values {
            let disk = ConfigDisk::encode(&[("ORB_TOKEN", "t"), ("AMP_URL", value)])
                .expect("these values are encodable");
            let parsed = parse(disk.as_bytes()).expect("an encoded disk parses");

            assert_eq!(
                parsed,
                [
                    ("ORB_TOKEN".to_owned(), "t".to_owned()),
                    ("AMP_URL".to_owned(), value.clone())
                ],
                "{value:?} did not survive the trip"
            );
        }
    }

    #[test]
    fn a_disk_is_its_lines_padded_to_a_fixed_size() {
        let disk = ConfigDisk::encode(&[("ORB_TOKEN", "a-token")]).unwrap();

        assert_eq!(disk.as_bytes().len(), BYTES);
        assert_eq!(&disk.as_bytes()[..18], b"ORB_TOKEN=a-token\n");
        assert!(disk.as_bytes()[18..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn what_could_not_be_read_back_is_refused_without_the_value() {
        for value in ["two\nlines", "a\0b", "trailing\n"] {
            let refusal = ConfigDisk::encode(&[("ORB_TOKEN", value)]).unwrap_err();
            assert_eq!(
                refusal,
                Error::Value {
                    key: "ORB_TOKEN".to_owned()
                }
            );
            assert!(!refusal.to_string().contains(value));
        }

        for key in ["", "A=B", "A\nB", "A\0B"] {
            assert!(matches!(
                ConfigDisk::encode(&[(key, "v")]),
                Err(Error::Key { .. })
            ));
        }

        assert_eq!(
            ConfigDisk::encode(&[("ORB_TOKEN", &"a".repeat(BYTES))]),
            Err(Error::TooLarge {
                needed: BYTES + "ORB_TOKEN=\n".len()
            })
        );
    }

    #[test]
    fn reading_stops_at_the_padding_and_at_the_block() {
        let mut disk = b"\nORB_TOKEN=secret\n\n".to_vec();
        disk.resize(BYTES, 0);
        disk.extend_from_slice(b"ORB_TOKEN=not-this-one\n");

        assert_eq!(
            parse(&disk).unwrap(),
            [("ORB_TOKEN".to_owned(), "secret".to_owned())]
        );
        assert_eq!(parse(&[0; BYTES]).unwrap(), []);
    }

    #[test]
    fn a_disk_that_is_not_ours_is_refused() {
        assert_eq!(
            parse(b"ORB_TOKEN\n"),
            Err(Error::NotAnAssignment { line: 1 })
        );
        assert_eq!(parse(&[0xff, b'\n']), Err(Error::NotUtf8));
    }

    #[test]
    fn the_contents_never_appear_in_debug_output() {
        let disk = ConfigDisk::encode(&[("ORB_TOKEN", "must-not-leak")]).unwrap();
        assert!(!format!("{disk:?}").contains("must-not-leak"));
    }
}
