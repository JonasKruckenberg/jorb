//! The variables jorb is given: `ORB_*` and `AMP_URL` from Amp, `JORB_*` from
//! the root-side wrapper. They are read from the process once, so the parsers
//! take plain data, and tests can hand them some without setting the process
//! environment, which is unsafe.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

pub struct Variables(HashMap<String, String>);

impl Variables {
    /// One of jorb's variables that is not UTF-8 is an error rather than
    /// absent; anything else in the environment is not jorb's business.
    pub fn from_process() -> Result<Self> {
        let mut variables = HashMap::new();
        for (name, value) in std::env::vars_os() {
            let Ok(name) = name.into_string() else {
                continue;
            };
            if !(name.starts_with("ORB_") || name.starts_with("JORB_") || name == "AMP_URL") {
                continue;
            }
            match value.into_string() {
                Ok(value) => variables.insert(name, value),
                Err(_) => bail!("{name} is not UTF-8"),
            };
        }
        Ok(Self(variables))
    }

    pub fn required(&self, name: &str) -> Result<&str> {
        self.optional(name)
            .with_context(|| format!("{name} is not set"))
    }

    pub fn optional(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    #[cfg(test)]
    pub fn of(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect(),
        )
    }
}
