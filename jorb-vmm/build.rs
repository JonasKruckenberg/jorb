//! Generates the Cloud Hypervisor client from `$CH_OPENAPI_SPEC`, which the
//! flake points at the spec inside the exact package it installs, so client
//! and VMM cannot drift. Only the operations jorb calls are kept, which also
//! keeps out the parts of the spec progenitor cannot take.
#![allow(
    clippy::print_stdout,
    reason = "cargo reads its directives from stdout"
)]

use std::error::Error;
use std::{env, fs, path::PathBuf};

use openapiv3::{OpenAPI, ReferenceOr};

/// Each path jorb calls, and the name its method gets. `/vm.info` has none in
/// the spec.
const USED: [(&str, &str); 3] = [
    ("/vm.create", "createVM"),
    ("/vm.boot", "bootVM"),
    ("/vm.info", "vmInfo"),
];

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-env-changed=CH_OPENAPI_SPEC");
    let path = env::var("CH_OPENAPI_SPEC")
        .map_err(|_| "CH_OPENAPI_SPEC is not set; build in the dev shell")?;
    println!("cargo::rerun-if-changed={path}");

    let mut spec: OpenAPI = serde_yaml::from_reader(fs::File::open(&path)?)?;
    spec.paths
        .paths
        .retain(|path, _| USED.iter().any(|(used, _)| used == path));
    for (path, name) in USED {
        let Some(ReferenceOr::Item(item)) = spec.paths.paths.get_mut(path) else {
            return Err(format!("the spec has no {path}").into());
        };
        let operation = item
            .get
            .as_mut()
            .or(item.put.as_mut())
            .ok_or(format!("{path} has no operation"))?;
        operation
            .operation_id
            .get_or_insert_with(|| name.to_owned());
    }

    let mut settings = progenitor::GenerationSettings::default();
    settings.with_interface(progenitor::InterfaceStyle::Builder);
    let client = progenitor::Generator::new(&settings).generate_tokens(&spec)?;
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("cloud_hypervisor.rs"),
        client.to_string(),
    )?;
    Ok(())
}
