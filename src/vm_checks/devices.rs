//! VM checks for attached disks and headless deadlines.
//!
//! `read-only-disk` boots an old Debian disk with a second read-only disk and
//! reads `/sys/block/vdb/ro` in the guest. `headless-timeout` boots an Alpine
//! clone whose script never sends `PREP-READY` and checks that `run_headless`
//! returns within the first-output, stop, and join budget.

use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use crate::cache::clone_file;
use crate::control::FrameReader;
use crate::session::LoginAction::*;
use crate::vm_config::{DiskSpec, SerialPort, SerialRole, VmSpec, output_serial};
use crate::vm_io::create_pipe;
use crate::vm_lifecycle::{
    FrameDecision, HeadlessDeadlines, STOP_TIMEOUT, WORKER_JOIN_TIMEOUT, run_headless,
};
use crate::vm_run::drive_interactive;

pub(super) fn run_read_only_disk() -> Result<(), Box<dyn std::error::Error>> {
    check_read_only_second_disk()
}

pub(super) fn run_headless_timeout() -> Result<(), Box<dyn std::error::Error>> {
    check_headless_timeout()
}

pub(super) fn work_dir(name: &str) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "vibe-check-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub(super) fn clone_disk(src: &Path, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let kind = clone_file(src, dest)?;
    fs::set_permissions(dest, fs::Permissions::from_mode(0o644))?;
    eprintln!("cloned {} -> {} ({kind:?})", src.display(), dest.display());
    Ok(())
}

pub(super) fn required_env(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => Err(format!("{name} is unset").into()),
    }
}

/// Boot an old Debian disk plus a second read-only raw disk and read
/// `/sys/block/vdb/ro` from the guest.
fn check_read_only_second_disk() -> Result<(), Box<dyn std::error::Error>> {
    let source = required_env("VIBE_TEST_DEBIAN_OLD_RAW")?;
    if !source.is_file() {
        return Err(format!("old Debian raw does not exist: {}", source.display()).into());
    }
    let work = work_dir("ro")?;
    let disk = work.join("disk.raw");
    let extra = work.join("extra.raw");
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        clone_disk(&source, &disk)?;
        let extra_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&extra)?;
        extra_file.set_len(16 * 1024 * 1024)?;
        drop(extra_file);

        let (vm_reads_from, we_write_to) = create_pipe();
        let (we_read_from, vm_writes_to) = create_pipe();
        let (resize_reads_from, we_write_resize_to) = create_pipe();
        let spec = VmSpec {
            disks: vec![
                DiskSpec {
                    path: disk.clone(),
                    read_only: false,
                },
                DiskSpec {
                    path: extra.clone(),
                    read_only: true,
                },
            ],
            network: None,
            share_devices: Vec::new(),
            serial_ports: vec![
                SerialPort {
                    role: SerialRole::Terminal,
                    read_fd: Some(vm_reads_from),
                    write_fd: Some(vm_writes_to),
                    host_read: None,
                },
                SerialPort {
                    role: SerialRole::Resize,
                    read_fd: Some(resize_reads_from),
                    write_fd: None,
                    host_read: None,
                },
            ],
            efi_vars: work.join("efi.efivars"),
            cpu_count: 2,
            ram_bytes: 2048 * 1024 * 1024,
        };

        eprintln!(
            "read-only-disk: booting {} with read-only {}",
            disk.display(),
            extra.display()
        );
        drive_interactive(
            spec,
            &disk,
            None,
            &[],
            &[
                Send(" echo VIBE_RO_$(cat /sys/block/vda/ro)_$(cat /sys/block/vdb/ro)".to_string()),
                Expect {
                    text: "VIBE_RO_0_1".to_string(),
                    timeout: Duration::from_secs(30),
                },
                Send(" systemctl poweroff".to_string()),
            ],
            0..3,
            we_read_from,
            we_write_to,
            we_write_resize_to,
            None,
        )?;
        eprintln!("read-only-disk: guest reported /sys/block/vda/ro=0 and /sys/block/vdb/ro=1");
        Ok(())
    })();

    if result.is_ok() {
        let _ = fs::remove_dir_all(&work);
    } else {
        eprintln!("read-only-disk: leaving {}", work.display());
    }
    result
}

/// Alpine clone plus a cidata ISO whose user-data prints a line and then sleeps.
/// `run_headless` must return on the shortened first-output timeout.
fn check_headless_timeout() -> Result<(), Box<dyn std::error::Error>> {
    let source = required_env("VIBE_TEST_ALPINE_RAW")?;
    if !source.is_file() {
        return Err(format!("Alpine raw does not exist: {}", source.display()).into());
    }
    let before = fs::metadata(&source)?;
    let work = work_dir("headless")?;
    let alpine = work.join("alpine.raw");
    let log_path = work.join("hvc0.log");
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        clone_disk(&source, &alpine)?;
        let iso = make_sleep_iso(&work)?;
        let first_output = Duration::from_secs(45);
        let deadlines = HeadlessDeadlines {
            first_output,
            total: Duration::from_secs(300),
            stop: STOP_TIMEOUT,
            worker_join: WORKER_JOIN_TIMEOUT,
        };
        let spec = VmSpec {
            disks: vec![
                DiskSpec {
                    path: alpine.clone(),
                    read_only: false,
                },
                DiskSpec {
                    path: iso,
                    read_only: true,
                },
            ],
            network: None,
            share_devices: Vec::new(),
            serial_ports: vec![
                output_serial(SerialRole::Log),
                output_serial(SerialRole::Control),
            ],
            efi_vars: work.join("efi.efivars"),
            cpu_count: 2,
            ram_bytes: 1024 * 1024 * 1024,
        };
        eprintln!(
            "headless-timeout: first_output={}s stop={}s join={}s",
            first_output.as_secs(),
            deadlines.stop.as_secs(),
            deadlines.worker_join.as_secs()
        );
        let wall = Instant::now();
        let outcome = run_headless(
            spec,
            &log_path,
            FrameReader::new("headlesstimeout"),
            deadlines,
            |_| FrameDecision::Continue,
        )?;
        let wall = wall.elapsed();
        let log = fs::read_to_string(&log_path).unwrap_or_default();
        eprintln!(
            "headless-timeout: elapsed {:.1}s (clock {:.1}s), forced={}, error={:?}, log_bytes={}",
            wall.as_secs_f64(),
            outcome.elapsed.as_secs_f64(),
            outcome.forced_stop,
            outcome.error,
            log.len()
        );
        let budget = deadlines.first_output + deadlines.stop + deadlines.worker_join;
        if outcome.elapsed > budget {
            return Err(format!(
                "headless run took {:.1}s, budget is {:.1}s (first_output + STOP_TIMEOUT + WORKER_JOIN_TIMEOUT)",
                outcome.elapsed.as_secs_f64(),
                budget.as_secs_f64()
            )
            .into());
        }
        if outcome.elapsed + Duration::from_secs(2) < deadlines.first_output {
            return Err(format!(
                "headless run returned in {:.1}s, before the {}s first-output timeout",
                outcome.elapsed.as_secs_f64(),
                deadlines.first_output.as_secs()
            )
            .into());
        }
        match outcome.error.as_deref() {
            Some(message)
                if message.contains("timed out waiting for PREP-READY")
                    && !message.contains("did not stop") => {}
            other => {
                return Err(
                    format!("expected a PREP-READY timeout, got {other:?}; log:\n{log}").into(),
                );
            }
        }
        if !log.contains("VIBE_HEADLESS_SLEEP") {
            return Err(format!("hvc0 log does not contain the script marker:\n{log}").into());
        }
        let after = fs::metadata(&source)?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err("Alpine raw length or mtime changed".into());
        }
        eprintln!(
            "headless-timeout: timeout path returned within the budget and the log has the script marker"
        );
        Ok(())
    })();

    if result.is_ok() {
        let _ = fs::remove_dir_all(&work);
    } else {
        eprintln!("headless-timeout: leaving {}", work.display());
    }
    result
}

fn make_sleep_iso(work: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let seed = work.join("seed");
    fs::create_dir_all(&seed)?;
    fs::write(
        seed.join("meta-data"),
        "instance-id: vibe-headless-timeout\nlocal-hostname: vibe-maint\n",
    )?;
    fs::write(
        seed.join("user-data"),
        "#!/bin/sh\necho VIBE_HEADLESS_SLEEP >/dev/hvc0\nsleep 1000\n",
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
