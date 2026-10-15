//! Image files in the cache directory: the downloaded Debian base image, named
//! images, and per-project instance disks.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) const DEBIAN_COMPRESSED_DISK_URL: &str = "https://cdimage.debian.org/cdimage/cloud/trixie/20261001-2618/debian-13-nocloud-arm64-20261001-2618.tar.xz";
const DEBIAN_COMPRESSED_SHA: &str = "c049582fe2d13d9e0c1a4767ade3a32bed217774bd6e39ac8fc60ca78534bfc258a8c52363ddfad639d5db7eacc2550688d5393f16aaf62bc5d19ecd4a7e6170";
const DEBIAN_COMPRESSED_FILE_NAME: &str = "disk.raw";
const DEBIAN_COMPRESSED_SIZE_BYTES: u64 = 280000000; // 280901576, 284558188

pub(crate) fn assert_valid_image_name(name: &str) {
    assert!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c == '-' || c == '_' || c.is_ascii_alphanumeric()),
        "Image name must be alphanumeric, dash, or underscore."
    );
}

pub(crate) fn image_path(cache_dir: &Path, name: &str) -> PathBuf {
    assert_valid_image_name(name);
    cache_dir.join(format!("{name}.raw"))
}

pub(crate) fn ensure_base_image(
    base_raw: &Path,
    base_compressed: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if base_raw.exists() {
        return Ok(());
    }

    if !base_compressed.exists()
        || std::fs::metadata(base_compressed).map(|m| m.len())? < DEBIAN_COMPRESSED_SIZE_BYTES
    {
        println!("Downloading base image...");
        let status = Command::new("curl")
            .args([
                "--continue-at",
                "-",
                "--compressed",
                "--location",
                "--fail",
                "-o",
                &base_compressed.to_string_lossy(),
                DEBIAN_COMPRESSED_DISK_URL,
            ])
            .status()?;
        if !status.success() {
            return Err("Failed to download base image".into());
        }
    }

    // Check SHA
    {
        let input = format!("{}  {}\n", DEBIAN_COMPRESSED_SHA, base_compressed.display());

        let mut child = Command::new("/usr/bin/shasum")
            .args(["--algorithm", "512", "--check"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("failed to spawn shasum");

        child
            .stdin
            .take()
            .expect("failed to open stdin")
            .write_all(input.as_bytes())
            .expect("failed to write to stdin");

        let status = child.wait().expect("failed to wait on child");
        if !status.success() {
            return Err(format!("SHA validation failed for {DEBIAN_COMPRESSED_DISK_URL}").into());
        }
    }

    println!("Decompressing base image...");
    let status = Command::new("tar")
        .args([
            "-xOf",
            &base_compressed.to_string_lossy(),
            DEBIAN_COMPRESSED_FILE_NAME,
        ])
        .stdout(std::fs::File::create(base_raw)?)
        .status()?;

    if !status.success() {
        return Err("Failed to decompress base image".into());
    }

    Ok(())
}

pub(crate) fn ensure_instance_disk(
    instance_raw: &Path,
    template_raw: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if instance_raw.exists() {
        println!(
            "The instance disk {} has been copied already",
            instance_raw.display()
        );
        return Ok(());
    }

    println!("Creating instance disk from {}...", template_raw.display());
    std::fs::create_dir_all(instance_raw.parent().unwrap())?;
    fs::copy(template_raw, instance_raw)?;
    Ok(())
}
