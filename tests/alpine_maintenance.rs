//! Alpine maintenance VM checks. Virtualization.framework runs on the process
//! main thread, so this spawns the `vibe` binary with
//! `VIBE_VM_CHECK=maintenance-all` instead of booting inside the test harness.
//!
//! The check needs `VIBE_TEST_DEBIAN_NEW_RAW`, `VIBE_TEST_DEBIAN_OLD_RAW`, and
//! `VIBE_TEST_ALPINE_RAW`. It downloads the pinned Alpine archive through the
//! ambient proxy into a temporary cache.

use std::process::Command;

fn run_check(which: &str) -> std::process::ExitStatus {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.env("VIBE_VM_CHECK", which);
    for name in [
        "VIBE_TEST_DEBIAN_NEW_RAW",
        "VIBE_TEST_DEBIAN_OLD_RAW",
        "VIBE_TEST_ALPINE_RAW",
        "VIBE_E2FS_SBIN",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "https_proxy",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        if let Ok(value) = std::env::var(name) {
            cmd.env(name, value);
        }
    }
    cmd.status().expect("failed to spawn vibe")
}

#[test]
#[ignore = "boots maintenance VMs; set the VIBE_TEST_*_RAW images"]
fn alpine_maintenance_prepares_and_rejects_disks() {
    for name in [
        "VIBE_TEST_DEBIAN_NEW_RAW",
        "VIBE_TEST_DEBIAN_OLD_RAW",
        "VIBE_TEST_ALPINE_RAW",
    ] {
        let path = std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        assert!(
            std::path::Path::new(&path).is_file(),
            "{name} is not a file: {path}"
        );
    }
    let status = run_check("maintenance-all");
    assert!(status.success(), "maintenance check failed: {status}");
}
