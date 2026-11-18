//! One request from Amp: read the variables, take the lock, apply the
//! operation, report. Exit 0 means the operation's postcondition was observed.
//!
//! jorb is crash-only: the start path is the recovery path, nothing is
//! cleaned up on the way out, and no error branch tidies anything.

mod command;
mod limits;
mod request;
mod settings;
mod systemd;
mod variables;
mod vm;

use std::fs::{File, OpenOptions, TryLockError};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use tokio::time::{sleep, timeout};
use tracing::{Level, error, info, info_span};

use crate::request::Request;
use crate::settings::Settings;
use crate::variables::Variables;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(Level::DEBUG)
        .with_writer(std::io::stderr)
        .init();

    let parsed = Variables::from_process().and_then(|variables| {
        ensure!(std::env::args_os().len() == 1, "jorb takes no arguments");
        Ok((Request::parse(&variables)?, Settings::parse(&variables)?))
    });
    let (request, settings) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            error!(
                outcome = "rejected",
                error = format!("{error:#}"),
                "bad request"
            );
            return ExitCode::FAILURE;
        }
    };

    let _span = info_span!("request", op = request.kind.name(), orb = %request.orb).entered();

    let started = Instant::now();
    let result = {
        let _lock = lock(&settings).await?;
        timeout(limits::REQUEST_DEADLINE, request.apply(&settings))
            .await
            .with_context(|| format!("not finished within {:?}", limits::REQUEST_DEADLINE))?
    };
    let elapsed_ms = started.elapsed().as_millis();

    match result {
        Ok(()) => {
            info!(outcome = "ok", elapsed_ms, "request done");
            ExitCode::SUCCESS
        }
        Err(error) => {
            error!(
                outcome = "failed",
                error = format!("{error:#}"),
                elapsed_ms,
                "request failed"
            );
            ExitCode::FAILURE
        }
    }
}

async fn lock(settings: &Settings) -> Result<File> {
    let path = settings.lock_path();
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;

    let deadline = Instant::now() + limits::LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                sleep(limits::LOCK_POLL).await;
            }
            Err(TryLockError::WouldBlock) => {
                bail!("another request held the lock for {:?}", limits::LOCK_WAIT)
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}
