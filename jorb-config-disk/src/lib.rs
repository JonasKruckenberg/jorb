//! The config disk, the whole interface between jorb and jorb-init, defined
//! once: `KEY=VALUE\n` lines in UTF-8, NUL-padded to [`BYTES`]. Nothing is
//! trimmed, so a value reads back exactly as written, and a value that could
//! not is refused. No error carries a value, because one is a token.

use std::fmt;

/// One block holds the keys many times over, and a fixed size makes
/// "identical config" a byte comparison.
pub const BYTES: usize = 4096;

/// Its `Debug` never shows the contents.
#[derive(PartialEq, Eq)]
pub struct ConfigDisk(Vec<u8>);

#[derive(Debug, PartialEq, Eq)]
pub struct Error(String);

impl ConfigDisk {
    /// Keys are the caller's constants; values are refused if they hold a
    /// newline or a NUL.
    pub fn encode(entries: &[(&str, &str)]) -> Result<Self, Error> {
        let mut disk = Vec::new();
        for (key, value) in entries {
            if value.contains(['\n', '\0']) {
                return Err(Error(format!(
                    "the value of {key} contains a newline or a NUL"
                )));
            }
            disk.extend_from_slice(format!("{key}={value}\n").as_bytes());
        }
        if disk.len() > BYTES {
            return Err(Error(format!(
                "the config disk needs {} of {BYTES} bytes",
                disk.len()
            )));
        }
        disk.resize(BYTES, 0);
        Ok(Self(disk))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// The entries in order, up to the first NUL. jorb-init reads the image's
/// `/etc/jorb-env`, which is the same lines unpadded, with this too.
pub fn parse(disk: &[u8]) -> Result<Vec<(String, String)>, Error> {
    let end = disk
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(disk.len());
    let contents = std::str::from_utf8(&disk[..end])
        .map_err(|_| Error("the config disk is not UTF-8".to_owned()))?;

    contents
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .ok_or_else(|| Error("a config disk line has no =".to_owned()))
        })
        .collect()
}

impl fmt::Debug for ConfigDisk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ConfigDisk({} bytes)", self.0.len())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every string of up to three characters chosen to break a parser that
    /// trims, splits on the wrong `=`, ends lines at `\r`, or assumes ASCII.
    #[test]
    fn every_encodable_value_reads_back_byte_identical() {
        const ALPHABET: [&str; 8] = ["a", " ", "=", "\r", "\t", "#", "é", "\u{1F600}"];
        let mut values = vec![String::new()];
        for _ in 0..3 {
            let longer: Vec<String> = values
                .iter()
                .flat_map(|value| {
                    ALPHABET
                        .iter()
                        .map(move |character| format!("{value}{character}"))
                })
                .collect();
            values.extend(longer);
        }
        values.sort();
        values.dedup();
        assert_eq!(values.len(), 1 + 8 + 64 + 512);

        for value in &values {
            let disk = ConfigDisk::encode(&[("ORB_TOKEN", "t"), ("AMP_URL", value)]).unwrap();
            assert_eq!(
                parse(disk.as_bytes()).unwrap(),
                [
                    ("ORB_TOKEN".into(), "t".into()),
                    ("AMP_URL".into(), value.clone())
                ]
            );
        }
    }

    #[test]
    fn a_disk_is_its_lines_padded_and_read_only_up_to_the_padding() {
        let disk = ConfigDisk::encode(&[("ORB_TOKEN", "secret")]).unwrap();
        assert_eq!(disk.as_bytes().len(), BYTES);
        assert!(disk.as_bytes()[17..].iter().all(|byte| *byte == 0));
        assert!(!format!("{disk:?}").contains("secret"));

        let mut longer = disk.as_bytes().to_vec();
        longer.extend_from_slice(b"ORB_TOKEN=not-this-one\n");
        assert_eq!(
            parse(&longer).unwrap(),
            [("ORB_TOKEN".into(), "secret".into())]
        );
    }

    #[test]
    fn what_could_not_be_read_back_is_refused_without_the_value() {
        for value in ["two\nlines", "a\0b"] {
            let refusal = ConfigDisk::encode(&[("ORB_TOKEN", value)])
                .unwrap_err()
                .to_string();
            assert!(
                refusal.contains("ORB_TOKEN") && !refusal.contains(value),
                "{refusal}"
            );
        }
        assert!(ConfigDisk::encode(&[("ORB_TOKEN", &"a".repeat(BYTES))]).is_err());
        assert!(parse(b"ORB_TOKEN\n").is_err());
        assert!(parse(&[0xff]).is_err());
    }
}
