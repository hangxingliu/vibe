use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use crate::{
    control::{FrameReader, new_nonce},
    networking::PreparedNetworkBackend,
    session::LoginAction::{Expect, Send},
    session_plan::SessionPlan,
    vm_config::{DiskSpec, SerialRole, VmSpec, output_serial},
    vm_lifecycle::{FrameDecision, HeadlessDeadlines, run_headless},
    vm_run::run_vm,
};

pub(super) fn legacy_boot(disk: &Path) -> Result<(), Box<dyn std::error::Error>> {
    run_vm(
        disk,
        None,
        None,
        &SessionPlan::root(
            vec![
                Send(" echo VIBE_SHELL_$(id -u)".into()),
                Expect {
                    text: "VIBE_SHELL_0".into(),
                    timeout: Duration::from_secs(60),
                },
                Send(" systemctl poweroff".into()),
            ],
            0..3,
        ),
        &[],
        |_| PreparedNetworkBackend::VzNat,
        2,
        2 * 1024 * 1024 * 1024,
        None,
        None,
    )?;
    Ok(())
}

pub(super) fn boot_disks(
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

pub(super) fn shadow_iso(work: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let seed = work.join("seed");
    fs::create_dir_all(&seed)?;
    fs::write(
        seed.join("meta-data"),
        "instance-id: vibe-shadow\nlocal-hostname: vibe-maint\n",
    )?;
    fs::write(seed.join("maint.sh"), include_str!("../../guest/maint.sh"))?;
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
