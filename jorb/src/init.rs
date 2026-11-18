//! Making a host ready to run orbs.
//!
//! Preflight asks; init changes the host and then asks, so what it leaves
//! behind is described by the same questions. Each step below creates
//! something and says so, and each is idempotent: a host that is already
//! ready comes out unchanged.
//!
//! Creating a filesystem is the one step that waits to be asked for, because
//! mounting something outlives the command that did it. The Nix module passes
//! `--create-state-image`, so a NixOS host still needs no attention.
#![allow(
    clippy::print_stdout,
    reason = "saying what changed is this module's output"
)]

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::constants;
use crate::host::{self, HostConfig};
use crate::preflight;

/// The filesystem init may create, when it was asked for one.
#[derive(Debug)]
pub struct StateImage {
    pub path: PathBuf,
    pub gib: u32,
}

/// Makes the host ready, then reports on it. `Ok(false)` means it is still
/// not ready and preflight has said why.
pub async fn init(host: &HostConfig, image: Option<&StateImage>) -> Result<bool> {
    preflight::root()
        .await
        .context("jorb init creates directories, filesystems and the base image")?;

    create_directories(host)?;
    create_state_filesystem(host, image).await?;
    build_base_image(host).await?;

    println!("--");
    preflight::preflight(host).await
}

/// The directories every job expects. Mode 0700 because a config disk under
/// the run directory holds a token.
fn create_directories(host: &HostConfig) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    for directory in [
        host.state_directory().to_path_buf(),
        host.disks_directory(),
        host.run_directory().to_path_buf(),
    ] {
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        std::fs::set_permissions(
            &directory,
            std::fs::Permissions::from_mode(constants::STATE_DIRECTORY_MODE),
        )
        .with_context(|| format!("setting the mode of {}", directory.display()))?;
        println!("ok   {}", directory.display());
    }

    Ok(())
}

/// Gives the state directory a filesystem that can clone, if it has not got
/// one and init was asked to.
///
/// The image is sparse, so it costs what the orbs write rather than its size.
/// The mount lasts until the host reboots; keeping it is the Nix module's job
/// on NixOS and an fstab line elsewhere, so this prints that line.
async fn create_state_filesystem(host: &HostConfig, image: Option<&StateImage>) -> Result<()> {
    let directory = host.state_directory();

    if preflight::can_clone(host) {
        println!("ok   {} can clone", directory.display());
        return Ok(());
    }

    let Some(image) = image else {
        println!(
            "--   {} cannot clone, so every spawn would copy 64 GiB. \
             Pass --create-state-image to mount XFS there, or mount a filesystem \
             that can clone on it yourself.",
            directory.display()
        );
        return Ok(());
    };

    if image.path.try_exists()? {
        println!("ok   {} is there", image.path.display());
    } else {
        create_image(image).await?;
    }

    host::run(
        constants::MOUNT,
        &[
            "-o",
            "loop",
            &image.path.to_string_lossy(),
            &directory.to_string_lossy(),
        ],
    )
    .await
    .with_context(|| {
        format!(
            "mounting {} on {}",
            image.path.display(),
            directory.display()
        )
    })?;

    println!(
        "ok   mounted on {}, until this host reboots. To keep it: the module's \
         stateImage option, or in fstab: {} {} xfs loop 0 0",
        directory.display(),
        image.path.display(),
        directory.display()
    );

    // The mount hid the directories made above.
    create_directories(host)
}

/// A sparse file with XFS on it. `mkfs.xfs` is the only way to make one, so
/// this is one of the two programs init runs.
async fn create_image(image: &StateImage) -> Result<()> {
    let file = std::fs::File::create(&image.path)
        .with_context(|| format!("creating {}", image.path.display()))?;
    file.set_len(u64::from(image.gib) * constants::BYTES_PER_GIB)
        .with_context(|| format!("sizing {}", image.path.display()))?;
    drop(file);

    host::run(
        constants::MKFS_XFS,
        &[
            "-q",
            "-m",
            "reflink=1",
            "-L",
            constants::STATE_LABEL,
            &image.path.to_string_lossy(),
        ],
    )
    .await
    .with_context(|| format!("putting XFS on {}", image.path.display()))?;

    println!("ok   {} is {} GiB of XFS", image.path.display(), image.gib);

    Ok(())
}

/// Builds the base image if it is missing, by name, so the PATH decides which
/// one as it does for the VMM.
async fn build_base_image(host: &HostConfig) -> Result<()> {
    let image = host.base_image();
    if image.try_exists()? {
        println!("ok   {} is there", image.display());
        return Ok(());
    }

    println!(
        "--   building {}, which pulls the Amp orb image",
        image.display()
    );
    host::run(constants::PREPARE_IMAGE, &[])
        .await
        .with_context(|| format!("running {}", constants::PREPARE_IMAGE))?;

    Ok(())
}
