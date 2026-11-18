//! The client progenitor writes from Cloud Hypervisor's OpenAPI spec.
//!
//! Private on purpose: every type here follows whatever the spec says, and the
//! rest of jorb must not depend on that shape. `build.rs` writes the file this
//! includes.
#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::dbg_macro,
    clippy::infinite_loop,
    missing_docs,
    unused,
    reason = "generated code is not held to this crate's conventions"
)]

include!(concat!(env!("OUT_DIR"), "/cloud_hypervisor.rs"));
