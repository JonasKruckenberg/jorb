//! A Cloud Hypervisor client, narrowed to the one VM shape jorb boots.
//!
//! The crate refuses to know about systemd, slots, orbs or Amp: it takes a
//! kernel, a cmdline, disks and a tap, and it returns a `Vm` that is running.
//! The generated client stays private so Cloud Hypervisor's API surface cannot
//! leak into callers.

mod limits;
