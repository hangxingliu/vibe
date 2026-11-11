//! Disk probe and preparation checks (`VIBE_VM_CHECK=probe` or `prepare`).
//! Not part of the CLI.
//!
//! `probe` reads `VIBE_VM_CHECK_DISK` and prints the class and machine-id fact.
//! It does not print shadow contents. `prepare` runs `ensure_bootable` on
//! `VIBE_VM_CHECK_DISK` with the Alpine cache at `VIBE_VM_CHECK_CACHE`.

use std::{env, path::PathBuf};

use crate::{
    disk_prepare::ensure_bootable,
    image_dirs::ImageDirs,
    image_probe::{self, MachineIdFact},
};

fn required(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => Err(format!("{name} is unset").into()),
    }
}

pub(super) fn check_probe() -> Result<(), Box<dyn std::error::Error>> {
    let disk = required("VIBE_VM_CHECK_DISK")?;
    let (class, facts) = image_probe::probe_disk(&disk);
    let root = match facts.root_password_empty {
        Some(true) => "empty",
        Some(false) => "not-empty",
        None => "unknown",
    };
    let machine = match facts.machine_id {
        MachineIdFact::Missing => "Missing",
        MachineIdFact::EmptyFile => "EmptyFile",
        MachineIdFact::Valid => "Valid",
        MachineIdFact::Uninitialized => "Uninitialized",
        MachineIdFact::Other => "Other",
        MachineIdFact::Symlink(_) => "Symlink",
    };
    println!("class={class}");
    println!("machine_id={machine}");
    println!("root_password_empty={root}");
    Ok(())
}

pub(super) fn check_prepare() -> Result<(), Box<dyn std::error::Error>> {
    let disk = required("VIBE_VM_CHECK_DISK")?;
    let cache = required("VIBE_VM_CHECK_CACHE")?;
    let kind = ensure_bootable(&disk, &ImageDirs::single(cache))?;
    println!("kind={kind}");
    Ok(())
}
