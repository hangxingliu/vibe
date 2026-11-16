use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::{
    alpine::{self, ALPINE_ARCHIVE_SHA512, ALPINE_IMAGE_NAME},
    image_dirs::ImageDirs,
    image_probe::{self, ImageClass},
    maintenance::process_disk,
    networking::PreparedNetworkBackend,
    session::LoginAction::{Script, Send},
    session_plan::SessionPlan,
    vm_config::DiskSpec,
    vm_run::run_vm,
};

use super::{
    EnvSet, RAW_SHA256,
    boot::{boot_disks, legacy_boot, shadow_iso},
    disk_read::{layout_of, read_kind, read_regular},
    e2fs::set_ext4_label,
    reject::assert_failure_log,
    util::{clone_disk, proxy_env, require_file, restore_proxy, set_proxy, sha256_file, work_dir},
    verify::{check_processed, snapshot},
};

pub(super) fn check_download() -> Result<(), Box<dyn std::error::Error>> {
    let cache = work_dir("download")?.join("cache");
    fs::create_dir_all(&cache)?;
    let dirs = ImageDirs::single(cache.clone());
    let saved = proxy_env();
    set_proxy("http://127.0.0.1:9");
    let offline = alpine::ensure_alpine_image(&dirs).unwrap_err();
    let text = offline.to_string();
    for needle in [
        "[download]",
        alpine::ALPINE_ARCHIVE_URL,
        ALPINE_ARCHIVE_SHA512,
    ] {
        if !text.contains(needle) {
            return Err(format!("offline error missing {needle}: {text}").into());
        }
    }
    let manual = cache.join(format!("{ALPINE_IMAGE_NAME}.raw.tar.gz"));
    if !text.contains(&manual.display().to_string()) {
        return Err(format!("offline error missing manual path: {text}").into());
    }
    eprintln!("maintenance download: offline error names the URL, SHA-512, and manual path");
    restore_proxy(&saved);

    let raw = alpine::ensure_alpine_image(&dirs)?;
    let sha = sha256_file(&raw)?;
    if sha != RAW_SHA256 {
        return Err(format!("downloaded raw SHA-256 is {sha}").into());
    }
    let mtime = fs::metadata(&raw)?.modified()?;
    eprintln!("maintenance download: fetched and extracted via the ambient proxy");

    set_proxy("http://127.0.0.1:9");
    let again = alpine::ensure_alpine_image(&dirs)?;
    if fs::metadata(&again)?.modified()? != mtime || sha256_file(&again)? != RAW_SHA256 {
        return Err("cache hit rewrote the Alpine raw".into());
    }
    eprintln!("maintenance download: warm cache did not use the network");
    restore_proxy(&saved);

    let archive = cache.join(format!("{ALPINE_IMAGE_NAME}.raw.tar.gz"));
    let mut bytes = fs::read(&archive)?;
    if bytes.is_empty() {
        return Err("archive is empty".into());
    }
    bytes[0] ^= 0xff;
    fs::write(&archive, &bytes)?;
    alpine::ensure_alpine_image(&dirs)?;
    crate::images::verify_sha512(&archive, ALPINE_ARCHIVE_SHA512)?;
    if sha256_file(&raw)? != RAW_SHA256 {
        return Err("re-download changed a valid raw".into());
    }
    eprintln!("maintenance download: corrupt archive was replaced");

    fs::remove_file(&raw)?;
    set_proxy("http://127.0.0.1:9");
    let extracted = alpine::ensure_alpine_image(&dirs)?;
    if sha256_file(&extracted)? != RAW_SHA256 {
        return Err("extract-only raw does not match".into());
    }
    eprintln!("maintenance download: missing raw was extracted from the cached archive");
    restore_proxy(&saved);
    Ok(())
}

pub(super) fn check_shadow(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir("shadow")?;
    let clone = work.join("alpine.raw");
    clone_disk(alpine, &clone)?;
    let iso = shadow_iso(&work)?;
    let log = work.join("hvc0.log");
    let text = boot_disks(
        &work,
        vec![
            DiskSpec {
                path: clone,
                read_only: false,
            },
            DiskSpec {
                path: iso,
                read_only: true,
            },
        ],
        &log,
    )?;
    if !text.contains("SHADOW_SELFTEST_OK") {
        return Err(format!("shadow selftest did not pass:\n{text}").into());
    }
    if text.contains("SHADOW_SELFTEST_FAIL") {
        return Err(format!("shadow selftest reported a failure:\n{text}").into());
    }
    eprintln!("maintenance shadow: Alpine selftest passed");
    Ok(())
}

pub(super) fn check_prepare(
    alpine: &Path,
    env_name: &str,
    boot: bool,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let source = require_file(env_name)?;
    let work = work_dir(env_name)?;
    let disk = work.join("disk.raw");
    clone_disk(&source, &disk)?;
    let cache = work.join("cache");
    let before = snapshot(&disk)?;
    process_disk(&disk, alpine, &cache, &cache)?;
    check_processed(&disk, &before)?;
    eprintln!("maintenance prepare: {env_name} processed");
    if boot {
        legacy_boot(&disk)?;
        eprintln!("maintenance prepare: {env_name} legacy login reached a root shell");
    }
    Ok(disk)
}

pub(super) fn check_again(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let disk = check_prepare(alpine, "VIBE_TEST_DEBIAN_NEW_RAW", false)?;
    check_again_on(alpine, &disk)
}

pub(super) fn check_again_on(alpine: &Path, disk: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let cache = disk.parent().unwrap().join("cache-again");
    let shadow = read_regular(disk, "/etc/shadow")?;
    process_disk(disk, alpine, &cache, &cache)?;
    let again = read_regular(disk, "/etc/shadow")?;
    if shadow != again {
        return Err("second preparation changed /etc/shadow".into());
    }
    eprintln!("maintenance prepare-again: shadow bytes unchanged");
    Ok(())
}

pub(super) fn check_recovery(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = require_file("VIBE_TEST_DEBIAN_OLD_RAW")?;
    let work = work_dir("recovery")?;
    let disk = work.join("disk.raw");
    clone_disk(&source, &disk)?;
    let _guard = EnvSet::set("VIBE_FORCE_STOP_AFTER_LOGIN", "1");
    let booted = run_vm(
        &disk,
        None,
        None,
        &SessionPlan::root(Vec::new(), 0..0),
        &[],
        |_| PreparedNetworkBackend::VzNat,
        2,
        2 * 1024 * 1024 * 1024,
        None,
        None,
    );
    if booted.is_ok() {
        return Err("force-stop hook returned success".into());
    }
    let (class, _) = image_probe::probe_disk(&disk);
    let ImageClass::Unknown(reason) = &class else {
        return Err(format!("dirty disk classified as {class}").into());
    };
    if !reason.contains("recovery") {
        return Err(format!("dirty disk reason is {reason}").into());
    }
    eprintln!("maintenance recovery: probe is unknown ({reason})");
    process_disk(&disk, alpine, &work.join("cache"), &work.join("cache"))?;
    eprintln!("maintenance recovery: e2fsck replay then preparation succeeded");
    Ok(())
}

pub(super) fn check_ssh(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let disk = check_prepare(alpine, "VIBE_TEST_DEBIAN_NEW_RAW", false)?;
    check_ssh_on(alpine, &disk)
}

pub(super) fn check_ssh_on(_alpine: &Path, disk: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let copy = disk.with_file_name("ssh.raw");
    clone_disk(disk, &copy)?;
    let has_sshd = read_kind(&copy, "/usr/sbin/sshd")?.is_some();
    let proxy = env::var("HTTPS_PROXY")
        .or_else(|_| env::var("https_proxy"))
        .or_else(|_| env::var("HTTP_PROXY"))
        .or_else(|_| env::var("http_proxy"))
        .unwrap_or_default();
    let proxy = proxy.replace('\'', "");
    let script = format!(
        r#"
set -u
ssh_check() {{
if ! [ -x /usr/sbin/sshd ]; then
  if [ -z '{proxy}' ]; then
    echo SSH_CHECK_SKIPPED no-proxy
    exit 0
  fi
  export http_proxy='{proxy}' HTTP_PROXY='{proxy}' https_proxy='{proxy}' HTTPS_PROXY='{proxy}'
  export DEBIAN_FRONTEND=noninteractive
  if ! timeout 180 apt-get update || ! timeout 300 apt-get install -y openssh-server openssh-client; then
    echo SSH_CHECK_SKIPPED apt-failed
    exit 0
  fi
fi
line=$(/usr/sbin/sshd -T | awk 'tolower($1)=="permitemptypasswords" {{print tolower($0); exit}}')
echo "SSHD_T $line"
if [ "$line" != "permitemptypasswords no" ]; then
  echo SSHD_T_MISMATCH
  exit 0
fi
mkdir -p /run/sshd
/usr/sbin/sshd || true
printf '%s\n' '#!/bin/sh' 'exit 0' > /tmp/vibe-askpass
chmod 755 /tmp/vibe-askpass
export SSH_ASKPASS=/tmp/vibe-askpass
export SSH_ASKPASS_REQUIRE=force
export DISPLAY=vibe
if timeout 20 setsid ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    -o NumberOfPasswordPrompts=1 -o ConnectTimeout=8 \
    root@127.0.0.1 echo SSH_EMPTY_UNEXPECTED_OK </dev/null; then
  echo SSH_EMPTY_UNEXPECTED_OK
  exit 0
fi
echo SSH_EMPTY_REJECTED
exit 0
}}
ssh_check 2>&1 | tee /run/vibe-ssh-check.log
"#,
        proxy = proxy
    );
    let outcome = run_vm(
        &copy,
        None,
        None,
        &SessionPlan::root(
            vec![
                Script {
                    name: "sshd".into(),
                    content: script,
                },
                // Matching the script's end marker drains its output from the
                // monitor, so print the result lines again before powering off.
                Send(" grep -E '^(SSH|SSHD)_' /run/vibe-ssh-check.log; systemctl poweroff".into()),
            ],
            0..2,
        ),
        &[],
        |_| PreparedNetworkBackend::VzNat,
        2,
        2 * 1024 * 1024 * 1024,
        None,
        None,
    )?;
    let output = outcome.output;
    if output.contains("SSH_EMPTY_UNEXPECTED_OK") {
        return Err("empty-password SSH succeeded".into());
    }
    if output.contains("SSH_CHECK_SKIPPED") {
        eprintln!("maintenance ssh: {output}");
        if has_sshd {
            return Err("sshd is installed but the check was skipped".into());
        }
        eprintln!("maintenance ssh: base image has no sshd and the guest install did not finish");
        return Ok(());
    }
    if !output.contains("permitemptypasswords no") || !output.contains("SSH_EMPTY_REJECTED") {
        return Err(format!("ssh check output was incomplete:\n{output}").into());
    }
    eprintln!(
        "maintenance ssh: sshd -T reports permitemptypasswords no and empty SSH was rejected"
    );
    Ok(())
}

pub(super) fn check_logs(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = require_file("VIBE_TEST_DEBIAN_NEW_RAW")?;
    let work = work_dir("logs")?;
    let disk = work.join("label.raw");
    clone_disk(&source, &disk)?;
    let layout = layout_of(&disk)?;
    set_ext4_label(&disk, layout.root.first_lba, "/")?;
    let err = process_disk(
        &disk,
        alpine,
        &work.join("cache-label"),
        &work.join("cache-label"),
    )
    .unwrap_err();
    let text = err.to_string();
    if !text.contains("[prepare] safety:") || !text.contains("tune2fs -L") {
        return Err(format!("label error was {text}").into());
    }
    eprintln!("maintenance logs: host label rejection stage is safety");

    let disk = work.join("part.raw");
    clone_disk(&source, &disk)?;
    let cache = work.join("cache-part");
    let _guard = EnvSet::set(
        "VIBE_MAINT_TEST_PARTUUID",
        "11111111-2222-3333-4444-555555555555",
    );
    let err = process_disk(&disk, alpine, &cache, &cache).unwrap_err();
    drop(_guard);
    assert_failure_log(
        &err,
        "target",
        Some("self"),
        &["maint: self ok", "maint: failed target"],
        &["!unprovisioned"],
    )?;
    eprintln!("maintenance logs: target failure matches the log and does not leak shadow");
    Ok(())
}
