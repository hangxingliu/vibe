use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use crate::{ext4ro::FileRead, gpt, maintenance::process_disk};

use super::{
    EnvSet,
    disk_read::{layout_of, read_any, read_kind, read_regular},
    e2fs::{debugfs_out, debugfs_script, inode_flags, set_ext4_label, with_root},
    util::{clone_disk, require_file, sha256_file, work_dir},
};

pub(super) fn check_negative(alpine: &Path) -> Result<(), Box<dyn std::error::Error>> {
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

pub(super) fn assert_failure_log(
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
    let err = process_disk(&disk, alpine, &work.join("cache"), &work.join("cache")).unwrap_err();
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
