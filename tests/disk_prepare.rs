//! Disk preparation checks for provision work copies. Virtualization.framework
//! runs on the process main thread, so this spawns the `vibe` binary with
//! `VIBE_VM_CHECK=prepare`.
//!
//! Needs `VIBE_TEST_DEBIAN_OLD_RAW`, `VIBE_TEST_DEBIAN_NEW_RAW`, and
//! `VIBE_TEST_ALPINE_RAW`. The Alpine archive is the `.raw.tar.gz` next to the
//! raw, when that file exists. The old-image check uses an empty cache and
//! must not download Alpine.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn prepare(disk: &Path, cache: &Path) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vibe"));
    cmd.env("VIBE_VM_CHECK", "prepare");
    cmd.env("VIBE_VM_CHECK_DISK", disk);
    cmd.env("VIBE_VM_CHECK_CACHE", cache);
    for name in [
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
    cmd.output().expect("failed to spawn vibe")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vibe-check-prepare-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn clone_disk(src: &Path, dest: &Path) {
    let status = Command::new("cp")
        .args(["-c"])
        .arg(src)
        .arg(dest)
        .status()
        .expect("cp");
    assert!(status.success(), "cp -c {} failed", src.display());
    fs::set_permissions(dest, fs::Permissions::from_mode(0o644)).unwrap();
}

fn sha256(path: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("shasum");
    assert!(output.status.success(), "shasum failed");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn require(name: &str) -> PathBuf {
    let path = std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let path = PathBuf::from(path);
    assert!(path.is_file(), "{name} is not a file: {}", path.display());
    path
}

#[test]
#[ignore = "prepares disk images; set VIBE_TEST_DEBIAN_OLD_RAW, VIBE_TEST_DEBIAN_NEW_RAW, and VIBE_TEST_ALPINE_RAW"]
fn old_base_skips_alpine_and_new_base_is_processed() {
    let old_src = require("VIBE_TEST_DEBIAN_OLD_RAW");
    let new_src = require("VIBE_TEST_DEBIAN_NEW_RAW");
    let alpine_src = require("VIBE_TEST_ALPINE_RAW");
    let old_meta = fs::metadata(&old_src).unwrap();
    let old_sha = sha256(&old_src);
    let new_meta = fs::metadata(&new_src).unwrap();
    let new_sha = sha256(&new_src);

    let empty_cache = work_dir("empty-cache");
    let old_copy = work_dir("old").join("work.raw");
    clone_disk(&old_src, &old_copy);
    let old_copy_sha = sha256(&old_copy);
    let started = std::time::Instant::now();
    let old = prepare(&old_copy, &empty_cache);
    let old_elapsed = started.elapsed();
    let old_out = String::from_utf8_lossy(&old.stdout);
    let old_err = String::from_utf8_lossy(&old.stderr);
    assert!(
        old.status.success(),
        "old prepare failed: {old_err}\n{old_out}"
    );
    assert!(
        old_out.contains("kind=AlreadyCompatible"),
        "stdout:\n{old_out}"
    );
    assert!(
        old_elapsed.as_secs() < 30,
        "old prepare took {:?}; it should not boot Alpine",
        old_elapsed
    );
    assert!(
        fs::read_dir(&empty_cache).unwrap().next().is_none(),
        "empty cache was written: {}",
        empty_cache.display()
    );
    assert_eq!(sha256(&old_copy), old_copy_sha, "old work copy changed");
    let old_after = fs::metadata(&old_src).unwrap();
    assert_eq!(old_sha, sha256(&old_src));
    assert_eq!(old_meta.len(), old_after.len());
    assert_eq!(old_meta.modified().unwrap(), old_after.modified().unwrap());

    let cache = work_dir("alpine-cache");
    let alpine_name = "alpine-3.24.2-aarch64-tiny-r1";
    let alpine_before = fs::metadata(&alpine_src).unwrap();
    fs::copy(&alpine_src, cache.join(format!("{alpine_name}.raw"))).unwrap();
    let mut archive_name = alpine_src.as_os_str().to_os_string();
    archive_name.push(".tar.gz");
    let archive_src = PathBuf::from(archive_name);
    if archive_src.is_file() {
        fs::copy(
            &archive_src,
            cache.join(format!("{alpine_name}.raw.tar.gz")),
        )
        .unwrap();
    }

    let new_copy = work_dir("new").join("work.raw");
    clone_disk(&new_src, &new_copy);
    let prepared = prepare(&new_copy, &cache);
    let prepared_out = String::from_utf8_lossy(&prepared.stdout);
    let prepared_err = String::from_utf8_lossy(&prepared.stderr);
    assert!(
        prepared.status.success(),
        "new prepare failed: {prepared_err}\n{prepared_out}"
    );
    assert!(
        prepared_out.contains("kind=Processed"),
        "stdout:\n{prepared_out}"
    );
    assert!(
        prepared_out.contains("empty password"),
        "missing empty-root notice:\n{prepared_out}"
    );

    let again_started = std::time::Instant::now();
    let again = prepare(&new_copy, &cache);
    let again_elapsed = again_started.elapsed();
    let again_out = String::from_utf8_lossy(&again.stdout);
    let again_err = String::from_utf8_lossy(&again.stderr);
    assert!(
        again.status.success(),
        "second prepare failed: {again_err}\n{again_out}"
    );
    assert!(
        again_out.contains("kind=AlreadyCompatible"),
        "processed copy was prepared again:\n{again_out}\n{again_err}"
    );
    assert!(
        again_elapsed.as_secs() < 30,
        "second prepare took {:?}; a processed disk must not boot Alpine",
        again_elapsed
    );

    let new_after = fs::metadata(&new_src).unwrap();
    assert_eq!(new_sha, sha256(&new_src));
    assert_eq!(new_meta.len(), new_after.len());
    assert_eq!(new_meta.modified().unwrap(), new_after.modified().unwrap());
    let alpine_after = fs::metadata(&alpine_src).unwrap();
    assert_eq!(alpine_before.len(), alpine_after.len());
    assert_eq!(
        alpine_before.modified().unwrap(),
        alpine_after.modified().unwrap()
    );

    let _ = fs::remove_dir_all(&empty_cache);
    let _ = fs::remove_dir_all(old_copy.parent().unwrap());
    let _ = fs::remove_dir_all(&cache);
    let _ = fs::remove_dir_all(new_copy.parent().unwrap());
}
