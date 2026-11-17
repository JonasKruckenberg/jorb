//! Generates the Cloud Hypervisor client from the OpenAPI spec at
//! `$CH_OPENAPI_SPEC`.
//!
//! The flake points that variable at the spec inside the source of the exact
//! `cloud-hypervisor` package it installs, so the client and the binary it
//! talks to cannot drift. The generated code lands in `$OUT_DIR` and is
//! included by a private module, so none of it is part of this crate's
//! interface.
//!
//! Cloud Hypervisor's spec needs two patches before progenitor accepts it.
//! Both are below, each with the reason it exists, and both fail the build
//! rather than guess when the spec stops looking the way they expect.
#![allow(
    clippy::print_stdout,
    reason = "a build script's directives to cargo are lines on stdout"
)]

use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use openapiv3::{OpenAPI, Operation, PathItem, ReferenceOr, StatusCode};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo::rerun-if-env-changed=CH_OPENAPI_SPEC");

    let spec_path = env::var("CH_OPENAPI_SPEC").map_err(|_| {
        "CH_OPENAPI_SPEC is not set: build inside the dev shell, which points it \
         at the spec in cloud-hypervisor's source"
    })?;
    println!("cargo::rerun-if-changed={spec_path}");

    let mut spec: OpenAPI = serde_yaml::from_reader(fs::File::open(&spec_path)?)?;
    name_unnamed_operations(&mut spec)?;
    drop_empty_alternative_success_responses(&mut spec)?;

    let mut settings = progenitor::GenerationSettings::default();
    settings.with_interface(progenitor::InterfaceStyle::Builder);

    let client = progenitor::Generator::new(&settings).generate_tokens(&spec)?;

    let out = PathBuf::from(env::var("OUT_DIR")?).join("cloud_hypervisor.rs");
    fs::write(out, client.to_string())?;

    Ok(())
}

/// Gives every operation an `operationId`, which progenitor requires and
/// Cloud Hypervisor's spec leaves off 24 of its operations.
///
/// Each of those paths has exactly one method, so the path names the operation
/// unambiguously: `/vmm.ping` becomes `vmm_ping`. A path with two unnamed
/// methods would not, so this fails instead of guessing.
fn name_unnamed_operations(spec: &mut OpenAPI) -> Result<(), Box<dyn Error>> {
    for (path, item) in path_items(spec) {
        let from_path = path
            .trim_start_matches('/')
            .replace(|character: char| !character.is_ascii_alphanumeric(), "_");

        let mut named = 0;
        for operation in operations(item) {
            if operation.operation_id.is_some() {
                continue;
            }

            named += 1;
            if named > 1 {
                return Err(
                    format!("path {path} has two operations without an operation ID").into(),
                );
            }
            operation.operation_id = Some(from_path.clone());
        }
    }

    Ok(())
}

/// Removes the empty-bodied success response from the eight device hot-plug
/// operations, which declare both a 200 carrying `PciDeviceInfo` and a bare
/// 204.
///
/// progenitor cannot express an operation whose success has two different
/// bodies: it asserts on one response type per operation. The 204 is the
/// alternative that carries nothing, so dropping it is the smaller loss, and
/// jorb hot-plugs nothing in any case. An operation whose success responses
/// all carry bodies would need a real decision, so this fails on one.
fn drop_empty_alternative_success_responses(spec: &mut OpenAPI) -> Result<(), Box<dyn Error>> {
    for (path, item) in path_items(spec) {
        for operation in operations(item) {
            let successes: Vec<StatusCode> = operation
                .responses
                .responses
                .keys()
                .filter(
                    |status| matches!(status, StatusCode::Code(code) if (200..300).contains(code)),
                )
                .cloned()
                .collect();

            if successes.len() < 2 {
                continue;
            }

            let empty: Vec<StatusCode> = successes
                .iter()
                .filter(|status| match operation.responses.responses.get(*status) {
                    Some(ReferenceOr::Item(response)) => response.content.is_empty(),
                    _ => false,
                })
                .cloned()
                .collect();

            if empty.len() == successes.len() || empty.is_empty() {
                return Err(format!(
                    "path {path} has {} success responses and {} of them are empty, so there is \
                     no obviously redundant one to drop",
                    successes.len(),
                    empty.len()
                )
                .into());
            }

            for status in empty {
                operation.responses.responses.shift_remove(&status);
            }
        }
    }

    Ok(())
}

/// The spec's paths, skipping any that is a reference. Cloud Hypervisor's spec
/// has none, and a reference would need resolving before either patch could
/// see the operations behind it.
fn path_items(spec: &mut OpenAPI) -> impl Iterator<Item = (&String, &mut PathItem)> {
    spec.paths
        .paths
        .iter_mut()
        .filter_map(|(path, entry)| match entry {
            ReferenceOr::Item(item) => Some((path, item)),
            ReferenceOr::Reference { .. } => None,
        })
}

/// The operations a path item actually defines.
fn operations(item: &mut PathItem) -> impl Iterator<Item = &mut Operation> {
    item.get
        .iter_mut()
        .chain(item.put.iter_mut())
        .chain(item.post.iter_mut())
        .chain(item.delete.iter_mut())
        .chain(item.options.iter_mut())
        .chain(item.head.iter_mut())
        .chain(item.patch.iter_mut())
        .chain(item.trace.iter_mut())
}
