//! VM checks for the Alpine maintenance path (`VIBE_VM_CHECK=maintenance-<name>`).
//!
//! `cargo test` does not run on the process main thread, so
//! `tests/alpine_maintenance.rs` spawns this binary with `VIBE_VM_CHECK`.
//! Not part of the CLI. Disks and the Alpine cache used here are temporary.

mod boot;
mod checks;
mod disk_read;
mod e2fs;
mod reject;
mod util;
mod verify;

use std::{env, fs, os::unix::fs::PermissionsExt, path::Path};

use checks::{
    check_again, check_again_on, check_download, check_logs, check_prepare, check_recovery,
    check_shadow, check_ssh, check_ssh_on,
};
use reject::check_negative;
use util::{require_file, sha256_file};

const RAW_SHA256: &str = "2fcc39f53fda58b63a31e8cd4d34f2d9b8dad902175053c41fb4f8806c68bf07";
const UNCHANGED: &[&str] = &[
    "/etc/passwd",
    "/etc/shadow-",
    "/etc/pam.d/common-auth",
    "/etc/pam.d/common-account",
    "/etc/pam.d/common-password",
    "/etc/pam.d/common-session",
    "/etc/pam.d/login",
    "/etc/pam.d/su",
    "/etc/ssh/sshd_config",
    "/boot/grub/grub.cfg",
];

struct EnvSet {
    key: &'static str,
    previous: Option<String>,
}

impl EnvSet {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var(key).ok();
        unsafe { env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvSet {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }
}

pub(super) fn run(which: &str) -> Result<(), Box<dyn std::error::Error>> {
    let alpine = require_file("VIBE_TEST_ALPINE_RAW")?;
    let before = fs::metadata(&alpine)?;
    let before_sha = sha256_file(&alpine)?;
    let result = match which {
        "download" => check_download(),
        "shadow" => check_shadow(&alpine),
        "prepare-new" => check_prepare(&alpine, "VIBE_TEST_DEBIAN_NEW_RAW", true).map(|_| ()),
        "prepare-old" => check_prepare(&alpine, "VIBE_TEST_DEBIAN_OLD_RAW", true).map(|_| ()),
        "prepare-again" => check_again(&alpine),
        "negative" => check_negative(&alpine),
        "needs-recovery" => check_recovery(&alpine),
        "ssh" => check_ssh(&alpine),
        "logs" => check_logs(&alpine),
        "all" => check_all(&alpine),
        other => Err(format!("unknown maintenance check '{other}'").into()),
    };
    let after = fs::metadata(&alpine)?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.permissions().mode() != after.permissions().mode()
    {
        return Err("Alpine raw metadata changed".into());
    }
    let after_sha = sha256_file(&alpine)?;
    if before_sha != after_sha {
        return Err("Alpine raw SHA-256 changed".into());
    }
    eprintln!("maintenance: Alpine raw mtime and SHA-256 unchanged");
    result
}

fn check_all(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    check_download()?;
    check_shadow(alpine)?;
    let new_disk = check_prepare(alpine, "VIBE_TEST_DEBIAN_NEW_RAW", true)?;
    check_prepare(alpine, "VIBE_TEST_DEBIAN_OLD_RAW", true)?;
    check_again_on(alpine, &new_disk)?;
    check_negative(alpine)?;
    check_recovery(alpine)?;
    check_ssh_on(alpine, &new_disk)?;
    check_logs(alpine)?;
    let _ = fs::remove_file(&new_disk);
    Ok(())
}
