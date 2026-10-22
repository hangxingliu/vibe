//! VM device checks: a read-only second disk and the headless first-output
//! timeout. Virtualization.framework runs on the process main thread, so these
//! spawn the `vibe` binary with `VIBE_VM_CHECK` instead of booting inside the
//! test harness.
//!
//! `read-only-disk` needs `VIBE_TEST_DEBIAN_OLD_RAW`.
//! `headless-timeout` needs `VIBE_TEST_ALPINE_RAW`.

use std::process::Command;

fn run_check(which: &str) -> std::process::ExitStatus {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.env("VIBE_VM_CHECK", which);
    for name in ["VIBE_TEST_DEBIAN_OLD_RAW", "VIBE_TEST_ALPINE_RAW"] {
        if let Ok(value) = std::env::var(name) {
            cmd.env(name, value);
        }
    }
    cmd.status().expect("failed to spawn vibe")
}

#[test]
#[ignore = "boots a VM; set VIBE_TEST_DEBIAN_OLD_RAW"]
fn second_disk_is_read_only_in_the_guest() {
    let path = std::env::var("VIBE_TEST_DEBIAN_OLD_RAW")
        .expect("set VIBE_TEST_DEBIAN_OLD_RAW to an old Debian raw image");
    assert!(
        std::path::Path::new(&path).is_file(),
        "VIBE_TEST_DEBIAN_OLD_RAW is not a file: {path}"
    );
    let status = run_check("read-only-disk");
    assert!(status.success(), "read-only-disk check failed: {status}");
}

#[test]
#[ignore = "boots a VM; set VIBE_TEST_ALPINE_RAW"]
fn headless_sleep_times_out_within_the_budget() {
    let path = std::env::var("VIBE_TEST_ALPINE_RAW")
        .expect("set VIBE_TEST_ALPINE_RAW to the Alpine tiny raw image");
    assert!(
        std::path::Path::new(&path).is_file(),
        "VIBE_TEST_ALPINE_RAW is not a file: {path}"
    );
    let status = run_check("headless-timeout");
    assert!(status.success(), "headless-timeout check failed: {status}");
}
