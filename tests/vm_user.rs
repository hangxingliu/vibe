//! End-to-end checks of `--vm-user` and `--root-password`. They provision
//! images and boot VMs through the `vibe` CLI, so they spawn the `vibe` binary
//! with `VIBE_VM_CHECK` (Virtualization.framework needs the process main
//! thread).
//!
//! Both need `VIBE_VM_CHECK_DISK`: a Vibe image or prepared Debian raw whose
//! root account has no password (for example a copy of `default.raw`). The
//! disk itself is not modified; the checks work on clones. Provisioning
//! installs `bindfs`, so the VM needs network access; put extra options such
//! as `--proxy socks5://HOST:PORT --dns 1.1.1.1` in `VIBE_VM_CHECK_ARGS`.

use std::process::Command;

fn run_check(which: &str) -> std::process::ExitStatus {
    let disk = std::env::var("VIBE_VM_CHECK_DISK")
        .expect("set VIBE_VM_CHECK_DISK to a raw image whose root has no password");
    assert!(
        std::path::Path::new(&disk).is_file(),
        "VIBE_VM_CHECK_DISK is not a file: {disk}"
    );
    Command::new(env!("CARGO_BIN_EXE_vibe"))
        .env("VIBE_VM_CHECK", which)
        .status()
        .expect("failed to spawn vibe")
}

#[test]
#[ignore = "provisions an image and boots VMs; set VIBE_VM_CHECK_DISK (and VIBE_VM_CHECK_ARGS)"]
fn vm_user_session_and_shares() {
    let status = run_check("vm-user");
    assert!(status.success(), "vm-user check failed: {status}");
}

#[test]
#[ignore = "provisions an image and boots VMs; set VIBE_VM_CHECK_DISK (and VIBE_VM_CHECK_ARGS)"]
fn root_password_login() {
    let status = run_check("root-password");
    assert!(status.success(), "root-password check failed: {status}");
}
