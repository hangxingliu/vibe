//! VM checks for the Alpine maintenance path (`VIBE_VM_CHECK=maintenance-<name>`).
//!
//! `cargo test` does not run on the process main thread, so
//! `tests/alpine_maintenance.rs` spawns this binary with `VIBE_VM_CHECK`.
//! Not part of the CLI. Disks and the Alpine cache used here are temporary.

use std::{
    env, fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime},
};

use crate::{
    alpine::{self, ALPINE_ARCHIVE_SHA512, ALPINE_IMAGE_NAME},
    cache::clone_file,
    control::{FrameReader, new_nonce},
    ext4ro::{Ext4Ro, FileMeta, FileRead},
    gpt::{self, Layout},
    image_probe::{self, ImageClass},
    maintenance::process_disk,
    networking::PreparedNetworkBackend,
    session::LoginAction::{Expect, Script, Send},
    vm_config::{DiskSpec, SerialRole, VmSpec, output_serial},
    vm_lifecycle::{FrameDecision, HeadlessDeadlines, run_headless},
    vm_run::run_vm,
};

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

fn check_download() -> Result<(), Box<dyn std::error::Error>> {
    let cache = work_dir("download")?.join("cache");
    fs::create_dir_all(&cache)?;
    let saved = proxy_env();
    set_proxy("http://127.0.0.1:9");
    let offline = alpine::ensure_alpine_image(&cache).unwrap_err();
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

    let raw = alpine::ensure_alpine_image(&cache)?;
    let sha = sha256_file(&raw)?;
    if sha != RAW_SHA256 {
        return Err(format!("downloaded raw SHA-256 is {sha}").into());
    }
    let mtime = fs::metadata(&raw)?.modified()?;
    eprintln!("maintenance download: fetched and extracted via the ambient proxy");

    set_proxy("http://127.0.0.1:9");
    let again = alpine::ensure_alpine_image(&cache)?;
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
    alpine::ensure_alpine_image(&cache)?;
    crate::images::verify_sha512(&archive, ALPINE_ARCHIVE_SHA512)?;
    if sha256_file(&raw)? != RAW_SHA256 {
        return Err("re-download changed a valid raw".into());
    }
    eprintln!("maintenance download: corrupt archive was replaced");

    fs::remove_file(&raw)?;
    set_proxy("http://127.0.0.1:9");
    let extracted = alpine::ensure_alpine_image(&cache)?;
    if sha256_file(&extracted)? != RAW_SHA256 {
        return Err("extract-only raw does not match".into());
    }
    eprintln!("maintenance download: missing raw was extracted from the cached archive");
    restore_proxy(&saved);
    Ok(())
}

fn check_shadow(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
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

fn check_prepare(
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
    process_disk(&disk, alpine, &cache)?;
    check_processed(&disk, &before)?;
    eprintln!("maintenance prepare: {env_name} processed");
    if boot {
        legacy_boot(&disk)?;
        eprintln!("maintenance prepare: {env_name} legacy login reached a root shell");
    }
    Ok(disk)
}

fn check_again(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let disk = check_prepare(alpine, "VIBE_TEST_DEBIAN_NEW_RAW", false)?;
    check_again_on(alpine, &disk)
}

fn check_again_on(alpine: &Path, disk: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let cache = disk.parent().unwrap().join("cache-again");
    let shadow = read_regular(disk, "/etc/shadow")?;
    process_disk(disk, alpine, &cache)?;
    let again = read_regular(disk, "/etc/shadow")?;
    if shadow != again {
        return Err("second preparation changed /etc/shadow".into());
    }
    eprintln!("maintenance prepare-again: shadow bytes unchanged");
    Ok(())
}

fn check_negative(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = require_file("VIBE_TEST_DEBIAN_NEW_RAW")?;
    reject_label(&source, alpine)?;
    reject_blank(alpine)?;
    reject_partition_type(&source, alpine)?;
    reject_partuuid(&source, alpine)?;
    reject_debugfs(
        &source,
        alpine,
        "machine-id-symlink",
        &[
            "rm /etc/machine-id",
            "symlink /etc/machine-id /tmp/outside-vibe",
        ],
        "inspect",
    )?;
    reject_shadow_file(&source, alpine, "shadow-two", b"root:locked-one:19999:0:99999:7:::\nroot:locked-two:19999:0:99999:7:::\ndaemon:*:19999:0:99999:7:::\n")?;
    reject_shadow_file(
        &source,
        alpine,
        "shadow-none",
        b"daemon:*:19999:0:99999:7:::\n",
    )?;
    reject_passwd(&source, alpine)?;
    reject_sshd(&source, alpine)?;
    reject_fsck(&source, alpine)?;
    eprintln!("maintenance negative: host and guest rejections happened before maintenance writes");
    Ok(())
}

fn check_recovery(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = require_file("VIBE_TEST_DEBIAN_OLD_RAW")?;
    let work = work_dir("recovery")?;
    let disk = work.join("disk.raw");
    clone_disk(&source, &disk)?;
    let _guard = EnvSet::set("VIBE_FORCE_STOP_AFTER_LOGIN", "1");
    let booted = run_vm(
        &disk,
        None,
        None,
        &[],
        0..0,
        &[],
        |_| PreparedNetworkBackend::VzNat,
        2,
        2 * 1024 * 1024 * 1024,
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
    process_disk(&disk, alpine, &work.join("cache"))?;
    eprintln!("maintenance recovery: e2fsck replay then preparation succeeded");
    Ok(())
}

fn check_ssh(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let disk = check_prepare(alpine, "VIBE_TEST_DEBIAN_NEW_RAW", false)?;
    check_ssh_on(alpine, &disk)
}

fn check_ssh_on(_alpine: &Path, disk: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
        &[
            Script {
                name: "sshd".into(),
                content: script,
            },
            // Matching the script's end marker drains its output from the
            // monitor, so print the result lines again before powering off.
            Send(" grep -E '^(SSH|SSHD)_' /run/vibe-ssh-check.log; systemctl poweroff".into()),
        ],
        0..2,
        &[],
        |_| PreparedNetworkBackend::VzNat,
        2,
        2 * 1024 * 1024 * 1024,
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

fn check_logs(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = require_file("VIBE_TEST_DEBIAN_NEW_RAW")?;
    let work = work_dir("logs")?;
    let disk = work.join("label.raw");
    clone_disk(&source, &disk)?;
    let layout = layout_of(&disk)?;
    set_ext4_label(&disk, layout.root.first_lba, "/")?;
    let err = process_disk(&disk, alpine, &work.join("cache-label")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &cache).unwrap_err();
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

fn assert_failure_log(
    err: &crate::maintenance::PrepareError,
    stage: &str,
    last_step: Option<&str>,
    has: &[&str],
    lacks: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let text = err.to_string();
    let expect = match last_step {
        Some(step) => format!("[prepare] {stage} (last PREP-STEP: {step}):"),
        None => format!("[prepare] {stage}:"),
    };
    if !text.contains(&expect) {
        return Err(format!("{stage} error was {text}").into());
    }
    let Some(log) = err.log.as_ref() else {
        return Err(format!("{stage} error has no log path: {text}").into());
    };
    if !text.contains(&log.display().to_string()) {
        return Err(format!("error does not name its log: {text}").into());
    }
    let body = fs::read_to_string(log)?;
    for needle in has {
        if !body.contains(needle) {
            return Err(format!("log for {stage} missing {needle}:\n{body}").into());
        }
    }
    if body.lines().any(|line| line.starts_with('+')) {
        return Err(format!("log for {stage} contains an xtrace:\n{body}").into());
    }
    for needle in lacks {
        if body.contains(needle) {
            return Err(format!("log for {stage} leaked {needle}:\n{body}").into());
        }
    }
    Ok(())
}

fn reject_label(source: &Path, alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir("neg-label")?;
    let disk = work.join("disk.raw");
    clone_disk(source, &disk)?;
    let layout = layout_of(&disk)?;
    set_ext4_label(&disk, layout.root.first_lba, "/")?;
    let hash = sha256_file(&disk)?;
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    if !err.to_string().contains("tune2fs -L") {
        return Err(format!("label rejection was {}", err).into());
    }
    if sha256_file(&disk)? != hash {
        return Err("label rejection wrote the disk".into());
    }
    eprintln!("maintenance negative: label / rejected before the VM");
    Ok(())
}

fn reject_blank(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir("neg-blank")?;
    let disk = work.join("blank.raw");
    let file = fs::File::create(&disk)?;
    file.set_len(8 * 1024 * 1024)?;
    drop(file);
    fs::set_permissions(&disk, fs::Permissions::from_mode(0o644))?;
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    if err.stage != "safety" {
        return Err(format!("blank disk error was {err}").into());
    }
    eprintln!("maintenance negative: non-GPT disk rejected by the host");
    Ok(())
}

fn reject_partition_type(source: &Path, alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir("neg-type")?;
    let disk = work.join("disk.raw");
    clone_disk(source, &disk)?;
    let layout = layout_of(&disk)?;
    gpt::set_partition_type_guid(
        &disk,
        layout.root.index,
        "11111111-1111-1111-1111-111111111111",
    )?;
    let hash = sha256_file(&disk)?;
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    if !err.to_string().contains("no Linux root") {
        return Err(format!("type change error was {err}").into());
    }
    if sha256_file(&disk)? != hash {
        return Err("partition-type rejection wrote the disk".into());
    }
    eprintln!("maintenance negative: unknown root type rejected by the host");
    Ok(())
}

fn reject_partuuid(source: &Path, alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir("neg-partuuid")?;
    let disk = work.join("disk.raw");
    clone_disk(source, &disk)?;
    let hash = sha256_file(&disk)?;
    let _guard = EnvSet::set(
        "VIBE_MAINT_TEST_PARTUUID",
        "11111111-2222-3333-4444-555555555555",
    );
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    assert_failure_log(
        &err,
        "target",
        Some("self"),
        &["maint: self ok", "maint: failed target"],
        &["!unprovisioned"],
    )?;
    if sha256_file(&disk)? != hash {
        return Err("partuuid rejection wrote the disk".into());
    }
    eprintln!("maintenance negative: PARTUUID mismatch rejected before writes");
    Ok(())
}

fn reject_debugfs(
    source: &Path,
    alpine: &Path,
    label: &str,
    commands: &[&str],
    stage: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir(&format!("neg-{label}"))?;
    let disk = work.join("disk.raw");
    clone_disk(source, &disk)?;
    with_root(&disk, |device| {
        debugfs_script(device, commands)?;
        Ok(())
    })?;
    match read_kind(&disk, "/etc/machine-id")? {
        Some(FileRead::Symlink(target)) if target == "/tmp/outside-vibe" => {}
        other => {
            return Err(format!(
                "{label} did not plant /etc/machine-id as a symlink to /tmp/outside-vibe ({other:?})"
            )
            .into());
        }
    }
    let machine = read_any(&disk, "/etc/machine-id")?;
    let shadow = read_regular(&disk, "/etc/shadow")?;
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    assert_failure_log(
        &err,
        stage,
        Some("fsck"),
        &["maint: fsck ok", "maint: failed inspect"],
        &["!unprovisioned"],
    )?;
    if read_any(&disk, "/etc/machine-id")? != machine
        || read_regular(&disk, "/etc/shadow")? != shadow
    {
        return Err(format!("{label} changed machine-id or shadow").into());
    }
    eprintln!("maintenance negative: {label} rejected at {stage}");
    Ok(())
}

fn reject_shadow_file(
    source: &Path,
    alpine: &Path,
    label: &str,
    contents: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir(&format!("neg-{label}"))?;
    let disk = work.join("disk.raw");
    clone_disk(source, &disk)?;
    let host_file = work.join("shadow");
    fs::write(&host_file, contents)?;
    with_root(&disk, |device| {
        debugfs_script(
            device,
            &[
                "rm /etc/shadow".to_string(),
                format!("write {} /etc/shadow", host_file.display()),
            ],
        )?;
        Ok(())
    })?;
    let shadow = read_regular(&disk, "/etc/shadow")?;
    let machine = read_any(&disk, "/etc/machine-id")?;
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    assert_failure_log(
        &err,
        "inspect",
        Some("fsck"),
        &["maint: fsck ok", "maint: failed inspect"],
        &["!unprovisioned", "locked-one", "locked-two"],
    )?;
    if read_regular(&disk, "/etc/shadow")? != shadow
        || read_any(&disk, "/etc/machine-id")? != machine
    {
        return Err(format!("{label} changed shadow or machine-id").into());
    }
    eprintln!("maintenance negative: {label} rejected at inspect");
    Ok(())
}

fn reject_passwd(source: &Path, alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir("neg-passwd")?;
    let disk = work.join("disk.raw");
    clone_disk(source, &disk)?;
    let mut passwd = read_regular(&disk, "/etc/passwd")?;
    let text = String::from_utf8_lossy(&passwd).replace("root:x:", "root:*:");
    passwd = text.into_bytes();
    let host_file = work.join("passwd");
    fs::write(&host_file, &passwd)?;
    with_root(&disk, |device| {
        debugfs_script(
            device,
            &[
                "rm /etc/passwd".to_string(),
                format!("write {} /etc/passwd", host_file.display()),
            ],
        )?;
        Ok(())
    })?;
    let machine = read_any(&disk, "/etc/machine-id")?;
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    assert_failure_log(
        &err,
        "inspect",
        Some("fsck"),
        &["maint: fsck ok", "maint: failed inspect"],
        &["!unprovisioned"],
    )?;
    if read_any(&disk, "/etc/machine-id")? != machine {
        return Err("passwd rejection truncated machine-id".into());
    }
    eprintln!("maintenance negative: passwd root field rejected at inspect");
    Ok(())
}

fn reject_sshd(source: &Path, alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir("neg-sshd")?;
    let disk = work.join("disk.raw");
    clone_disk(source, &disk)?;
    let host_file = work.join("sshd_config");
    fs::write(
        &host_file,
        b"# PermitEmptyPasswords yes\nPermitEmptyPasswords yes\n",
    )?;
    // Newer base images ship without openssh, so /etc/ssh may be missing.
    let mut commands = Vec::new();
    if read_kind(&disk, "/etc/ssh")?.is_none() {
        commands.push("mkdir /etc/ssh".to_string());
    } else if read_kind(&disk, "/etc/ssh/sshd_config")?.is_some() {
        commands.push("rm /etc/ssh/sshd_config".to_string());
    }
    commands.push(format!(
        "write {} /etc/ssh/sshd_config",
        host_file.display()
    ));
    with_root(&disk, |device| {
        debugfs_script(device, &commands)?;
        Ok(())
    })?;
    let machine = read_any(&disk, "/etc/machine-id")?;
    let shadow = read_regular(&disk, "/etc/shadow")?;
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    assert_failure_log(
        &err,
        "inspect",
        Some("fsck"),
        &["maint: fsck ok", "maint: failed inspect"],
        &["!unprovisioned", "PermitEmptyPasswords yes"],
    )?;
    if read_any(&disk, "/etc/machine-id")? != machine
        || read_regular(&disk, "/etc/shadow")? != shadow
    {
        return Err("sshd rejection changed machine-id or shadow".into());
    }
    eprintln!("maintenance negative: PermitEmptyPasswords yes rejected at inspect");
    Ok(())
}

fn reject_fsck(source: &Path, alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir("neg-fsck")?;
    let disk = work.join("disk.raw");
    clone_disk(source, &disk)?;
    with_root(&disk, |device| {
        let stat = debugfs_out(device, "stat /lost+found")?;
        let current = inode_flags(&stat)?;
        let imagic = current | 0x2000;
        debugfs_script(
            device,
            &[
                format!("set_inode_field /lost+found flags {imagic}"),
                "set_super_value state 2".into(),
            ],
        )?;
        Ok(())
    })?;
    let machine = read_any(&disk, "/etc/machine-id")?;
    let shadow = read_regular(&disk, "/etc/shadow")?;
    let err = process_disk(&disk, alpine, &work.join("cache")).unwrap_err();
    assert_failure_log(
        &err,
        "fsck",
        Some("target"),
        &["maint: target ok", "maint: failed fsck"],
        &["!unprovisioned"],
    )?;
    if read_any(&disk, "/etc/machine-id")? != machine
        || read_regular(&disk, "/etc/shadow")? != shadow
    {
        return Err("fsck rejection changed machine-id or shadow".into());
    }
    if machine != Some(b"uninitialized\n".to_vec()) {
        return Err("fsck case machine-id was not uninitialized".into());
    }
    eprintln!("maintenance negative: e2fsck -p exit 4 rejected before apply");
    Ok(())
}

fn check_processed(disk: &Path, before: &Snapshot) -> Result<(), Box<dyn std::error::Error>> {
    let after = snapshot(disk)?;
    if after.files != before.files {
        return Err("a file other than machine-id and shadow changed".into());
    }
    if after.esp_sha != before.esp_sha {
        return Err("ESP bytes changed".into());
    }
    if after.shadow_meta != before.shadow_meta {
        return Err(format!(
            "shadow metadata changed from {:?} to {:?}",
            before.shadow_meta, after.shadow_meta
        )
        .into());
    }
    if after.machine_meta != before.machine_meta {
        return Err(format!(
            "machine-id metadata changed from {:?} to {:?}",
            before.machine_meta, after.machine_meta
        )
        .into());
    }
    let mode = after.shadow_meta.mode & 0o7777;
    if mode != 0o640 || after.shadow_meta.uid != 0 {
        return Err(format!(
            "shadow mode/owner is {mode:o} uid {}",
            after.shadow_meta.uid
        )
        .into());
    }
    if let Some(gid) = before.shadow_gid
        && after.shadow_meta.gid != gid
    {
        return Err(format!(
            "shadow gid {} is not the shadow group {gid}",
            after.shadow_meta.gid
        )
        .into());
    }
    if !root_password_cleared(&before.shadow, &after.shadow) {
        return Err("shadow root password field was not the only change".into());
    }
    let machine = read_regular(disk, "/etc/machine-id")?;
    if !machine.is_empty() {
        return Err("machine-id was not truncated".into());
    }
    Ok(())
}

struct Snapshot {
    files: Vec<Option<Vec<u8>>>,
    shadow: Vec<u8>,
    shadow_meta: FileMeta,
    machine_meta: FileMeta,
    shadow_gid: Option<u32>,
    esp_sha: String,
}

fn snapshot(disk: &Path) -> Result<Snapshot, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    for path in UNCHANGED {
        files.push(read_any(disk, path)?);
    }
    let shadow = read_regular(disk, "/etc/shadow")?;
    let meta = lstat_required(disk, "/etc/shadow")?;
    let machine_meta = lstat_required(disk, "/etc/machine-id")?;
    let gid = shadow_group_gid(disk)?;
    let layout = layout_of(disk)?;
    let esp_off = layout.esp.first_lba * 512;
    let esp_len = (layout.esp.last_lba - layout.esp.first_lba + 1) * 512;
    Ok(Snapshot {
        files,
        shadow,
        shadow_meta: meta,
        machine_meta,
        shadow_gid: gid,
        esp_sha: sha256_range(disk, esp_off, esp_len)?,
    })
}

fn root_password_cleared(before: &[u8], after: &[u8]) -> bool {
    let before_lines: Vec<_> = split_lines(before);
    let after_lines: Vec<_> = split_lines(after);
    if before_lines.len() != after_lines.len() {
        return false;
    }
    let mut roots = 0u32;
    for (left, right) in before_lines.iter().zip(&after_lines) {
        if left.starts_with(b"root:") {
            roots += 1;
            let mut lb = split_fields(left);
            let rb = split_fields(right);
            if lb.len() != rb.len() || lb.len() < 2 || !rb[1].is_empty() {
                return false;
            }
            lb[1].clear();
            if lb != rb {
                return false;
            }
        } else if left != right {
            return false;
        }
    }
    roots == 1 && before.ends_with(b"\n") == after.ends_with(b"\n")
}

fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<_> = bytes.split(|b| *b == b'\n').collect();
    if bytes.ends_with(b"\n") {
        lines.pop();
    }
    lines
}

fn split_fields(line: &[u8]) -> Vec<Vec<u8>> {
    line.split(|b| *b == b':').map(|f| f.to_vec()).collect()
}

fn legacy_boot(disk: &Path) -> Result<(), Box<dyn std::error::Error>> {
    run_vm(
        disk,
        None,
        None,
        &[
            Send(" echo VIBE_SHELL_$(id -u)".into()),
            Expect {
                text: "VIBE_SHELL_0".into(),
                timeout: Duration::from_secs(60),
            },
            Send(" systemctl poweroff".into()),
        ],
        0..3,
        &[],
        |_| PreparedNetworkBackend::VzNat,
        2,
        2 * 1024 * 1024 * 1024,
        None,
    )?;
    Ok(())
}

fn boot_disks(
    work: &Path,
    disks: Vec<DiskSpec>,
    log: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let spec = VmSpec {
        disks,
        network: None,
        share_devices: Vec::new(),
        serial_ports: vec![
            output_serial(SerialRole::Log),
            output_serial(SerialRole::Control),
        ],
        efi_vars: work.join("efivars"),
        cpu_count: 2,
        ram_bytes: 1024 * 1024 * 1024,
    };
    let _ = run_headless(
        spec,
        log,
        FrameReader::new(new_nonce()?),
        HeadlessDeadlines::maintenance(),
        |_| FrameDecision::Continue,
    )?;
    Ok(fs::read_to_string(log).unwrap_or_default())
}

fn shadow_iso(work: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let seed = work.join("seed");
    fs::create_dir_all(&seed)?;
    fs::write(
        seed.join("meta-data"),
        "instance-id: vibe-shadow\nlocal-hostname: vibe-maint\n",
    )?;
    fs::write(seed.join("maint.sh"), include_str!("../guest/maint.sh"))?;
    fs::write(
        seed.join("user-data"),
        r#"#!/bin/sh
exec >/dev/hvc0 2>&1
mkdir -p /mnt/seed
cidata=$(findfs LABEL=cidata 2>/dev/null || true)
if [ -n "$cidata" ] && mount -t iso9660 -o ro "$cidata" /mnt/seed 2>/dev/null; then
    :
else
    src=$(awk '$3 == "iso9660" { print $2; exit }' /proc/mounts)
    if [ -z "$src" ] || ! mount --bind "$src" /mnt/seed; then
        echo SHADOW_SELFTEST_FAIL seed-mount
        exit 1
    fi
fi
if [ ! -f /mnt/seed/maint.sh ]; then
    echo SHADOW_SELFTEST_FAIL missing-script
    ls -la /mnt/seed
    exit 1
fi
sh /mnt/seed/maint.sh shadow-selftest
echo WRAPPER_DONE
poweroff
"#,
    )?;
    let iso = work.join("seed.iso");
    let output = Command::new("hdiutil")
        .args([
            "makehybrid",
            "-quiet",
            "-iso",
            "-joliet",
            "-default-volume-name",
            "cidata",
            "-o",
        ])
        .arg(&iso)
        .arg(&seed)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "hdiutil makehybrid failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(iso)
}

fn with_root(
    disk: &Path,
    body: impl FnOnce(&str) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let layout = layout_of(disk)?;
    let (whole, _mount) = attach_raw(disk)?;
    let device = format!("{whole}s{}", layout.root.index);
    let result = body(&device);
    detach_whole(&whole);
    result
}

fn detach_whole(whole: &str) {
    let status = Command::new("hdiutil").args(["detach", whole]).status();
    if status.map(|status| status.success()).unwrap_or(false) {
        return;
    }
    let _ = Command::new("diskutil").args(["eject", whole]).status();
}

/// First whole-disk device in `hdiutil attach -nomount` output.
///
/// A line looks like `/dev/disk4           GUID_partition_scheme`.
/// Partition nodes (`/dev/disk4s1`) are not the whole disk. The device name
/// itself contains the letter `s` in `disk`, so that letter cannot be used
/// to tell a whole disk from a partition.
fn whole_disk_from_attach(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let token = line.split_whitespace().next()?;
        let rest = token.strip_prefix("/dev/disk")?;
        if !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()) {
            Some(token.to_string())
        } else {
            None
        }
    })
}

fn attach_raw(disk: &Path) -> Result<(String, String), Box<dyn std::error::Error>> {
    let output = Command::new("hdiutil")
        .args([
            "attach",
            "-nomount",
            "-imagekey",
            "diskimage-class=CRawDiskImage",
        ])
        .arg(disk)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "hdiutil attach failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let whole = whole_disk_from_attach(&text)
        .ok_or_else(|| format!("hdiutil attach did not name a disk:\n{text}"))?;
    Ok((whole, text))
}

fn debugfs_script(
    device: &str,
    commands: &[impl AsRef<str>],
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!("vibe-debugfs-{}.cmds", std::process::id()));
    fs::write(
        &path,
        commands
            .iter()
            .map(|c| c.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )?;
    let output = Command::new(e2fs_bin("debugfs"))
        .args(["-w", "-f"])
        .arg(&path)
        .arg(device)
        .output()?;
    let _ = fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // debugfs exits 0 even when a command in the script fails. The version
    // banner is also on stderr, so match the phrases a failed command prints.
    let command_failed = stderr.contains("File exists")
        || stderr.contains("already exists")
        || stderr.contains("No such file")
        || stderr.contains("File not found")
        || stderr.contains("while ");
    if !output.status.success() || command_failed {
        return Err(format!(
            "debugfs failed: {}{stderr}",
            String::from_utf8_lossy(&output.stdout),
        )
        .into());
    }
    Ok(())
}

fn debugfs_out(device: &str, command: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new(e2fs_bin("debugfs"))
        .args(["-R", command, device])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "debugfs {command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `Flags:` is on the inode line in debugfs 1.47 (`Inode: 11 ... Flags: 0x80000`).
fn inode_flags(stat: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let field = stat
        .lines()
        .find_map(|line| {
            let rest = line.split("Flags:").nth(1)?;
            rest.split_whitespace().next().map(str::to_string)
        })
        .ok_or("debugfs stat did not report flags")?;
    let digits = field.trim_start_matches("0x");
    Ok(u32::from_str_radix(digits, 16)?)
}

fn e2fs_bin(name: &str) -> PathBuf {
    if let Ok(dir) = env::var("VIBE_E2FS_SBIN") {
        return PathBuf::from(dir).join(name);
    }
    let bundled = PathBuf::from("/tmp/e2fs-install/sbin").join(name);
    if bundled.exists() {
        bundled
    } else {
        PathBuf::from(name)
    }
}

fn layout_of(disk: &Path) -> Result<Layout, Box<dyn std::error::Error>> {
    let mut file = fs::File::open(disk)?;
    let bytes = file.metadata()?.len();
    let gpt = gpt::read_gpt(&mut file)?;
    Ok(gpt::select_layout(&gpt, bytes)?)
}

fn open_root(disk: &Path) -> Result<(fs::File, u64, u64), Box<dyn std::error::Error>> {
    let layout = layout_of(disk)?;
    let offset = layout.root.first_lba * 512;
    let len = (layout.root.last_lba - layout.root.first_lba + 1) * 512;
    Ok((fs::File::open(disk)?, offset, len))
}

fn read_regular(disk: &Path, path: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    match read_any(disk, path)? {
        Some(bytes) => Ok(bytes),
        None => Err(format!("{path} is missing").into()),
    }
}

fn read_any(disk: &Path, path: &str) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    let (file, offset, len) = open_root(disk)?;
    let fs = Ext4Ro::open(&file, offset, len)?;
    match fs.read_file(path, 1024 * 1024)? {
        FileRead::Regular(bytes) => Ok(Some(bytes)),
        FileRead::Symlink(target) => Ok(Some(target.into_bytes())),
        FileRead::Missing => Ok(None),
        FileRead::Other => Ok(None),
    }
}

fn read_kind(disk: &Path, path: &str) -> Result<Option<FileRead>, Box<dyn std::error::Error>> {
    let (file, offset, len) = open_root(disk)?;
    let fs = Ext4Ro::open(&file, offset, len)?;
    match fs.read_file(path, 64)? {
        FileRead::Missing => Ok(None),
        other => Ok(Some(other)),
    }
}

fn lstat_required(disk: &Path, path: &str) -> Result<FileMeta, Box<dyn std::error::Error>> {
    let (file, offset, len) = open_root(disk)?;
    let fs = Ext4Ro::open(&file, offset, len)?;
    fs.lstat(path)?
        .ok_or_else(|| format!("{path} is missing").into())
}

fn shadow_group_gid(disk: &Path) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let Some(bytes) = read_any(disk, "/etc/group")? else {
        return Ok(None);
    };
    for line in split_lines(&bytes) {
        let fields = split_fields(line);
        if fields.first().map(Vec::as_slice) == Some(b"shadow") && fields.len() >= 3 {
            let text = String::from_utf8_lossy(&fields[2]);
            return Ok(text.parse().ok());
        }
    }
    Ok(None)
}

fn set_ext4_label(
    disk: &Path,
    first_lba: u64,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = [0u8; 16];
    let raw = label.as_bytes();
    bytes[..raw.len().min(16)].copy_from_slice(&raw[..raw.len().min(16)]);
    let mut file = fs::OpenOptions::new().write(true).open(disk)?;
    file.seek(SeekFrom::Start(first_lba * 512 + 1024 + 0x78))?;
    file.write_all(&bytes)?;
    Ok(())
}

fn clone_disk(src: &Path, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    clone_file(src, dest)?;
    fs::set_permissions(dest, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

fn require_file(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let value = env::var(name).map_err(|_| format!("{name} is unset"))?;
    let path = PathBuf::from(&value);
    if !path.is_file() {
        return Err(format!("{name} is not a file: {value}").into());
    }
    Ok(path)
}

fn work_dir(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = env::temp_dir().join(format!(
        "vibe-check-maint-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("/usr/bin/shasum")
        .args(["--algorithm", "256"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(format!("shasum failed for {}", path.display()).into());
    }
    Ok(String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string())
}

fn sha256_range(path: &Path, offset: u64, len: u64) -> Result<String, Box<dyn std::error::Error>> {
    let mut child = Command::new("/usr/bin/shasum")
        .args(["--algorithm", "256"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut stdin = child.stdin.take().ok_or("shasum stdin")?;
    let mut left = len;
    let mut buf = vec![0u8; 1024 * 1024];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            return Err("short read while hashing".into());
        }
        stdin.write_all(&buf[..n])?;
        left -= n as u64;
    }
    drop(stdin);
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err("shasum failed".into());
    }
    Ok(String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string())
}

struct SavedProxy {
    vars: Vec<(&'static str, Option<String>)>,
}

fn proxy_env() -> SavedProxy {
    SavedProxy {
        vars: [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "NO_PROXY",
            "no_proxy",
        ]
        .into_iter()
        .map(|key| (key, env::var(key).ok()))
        .collect(),
    }
}

fn set_proxy(proxy: &str) {
    unsafe {
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            env::set_var(key, proxy);
        }
    }
}

fn restore_proxy(saved: &SavedProxy) {
    unsafe {
        for (key, value) in &saved.vars {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{inode_flags, whole_disk_from_attach};

    #[test]
    fn attach_listing_selects_the_whole_disk() {
        let text = "\
/dev/disk4          \tGUID_partition_scheme          \t\n\
/dev/disk4s1        \tB921B045-1DF0-41C3-AF44-4C6F280\t\n\
/dev/disk4s15       \tEFI\n";
        assert_eq!(whole_disk_from_attach(text).as_deref(), Some("/dev/disk4"));
        assert_eq!(
            whole_disk_from_attach("/dev/disk10\n/dev/disk10s1\tLinux\n").as_deref(),
            Some("/dev/disk10")
        );
        assert_eq!(
            whole_disk_from_attach("/dev/disk4                                          \n")
                .as_deref(),
            Some("/dev/disk4")
        );
        assert_eq!(whole_disk_from_attach("/dev/disk4s1\n").as_deref(), None);
        assert_eq!(whole_disk_from_attach("").as_deref(), None);
    }

    #[test]
    fn debugfs_stat_flags_are_on_the_inode_line() {
        let stat = "\
Inode: 11   Type: directory    Mode:  0700   Flags: 0x80000\n\
Generation: 0    Version: 0x00000000:00000000\n";
        assert_eq!(inode_flags(stat).unwrap(), 0x80000);
        assert!(inode_flags("Inode: 11 Type: directory\n").is_err());
    }
}
