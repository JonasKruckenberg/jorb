//! Every bound this crate imposes, with the reason for its value.

use std::time::Duration;

/// How long one request to Cloud Hypervisor's API may take, connection
/// included.
///
/// The API is a unix socket on the same host and every call jorb makes is a
/// few hundred bytes of JSON, so anything slower than this is a VMM that has
/// stopped answering rather than one that is busy. Short enough that a job can
/// make several calls and still finish well inside Amp's five minutes.
pub const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
