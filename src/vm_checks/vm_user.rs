//! End-to-end checks for `--vm-user` and `--root-password`.
//!
//! These checks run the `vibe` CLI itself (the current executable without
//! `VIBE_VM_CHECK`) against copies of `VIBE_VM_CHECK_DISK`, and read the
//! guest console from its output. New images are provisioned into a private
//! work directory that is used as the preferred image directory. Extra CLI
//! options for every VM (for example `--proxy socks5://... --dns 1.1.1.1`)
//! come from `VIBE_VM_CHECK_ARGS`, split on whitespace. Provisioning installs
//! `bindfs`, so the VM needs network access.
//!
//! - `vm-user`: provisions user `dev` with a root password, then checks a
//!   missing user, the session switch, writable and read-only shares, the
//!   `.vibe` mask, the unmasked `.env`, the staging directory, `umount`,
//!   case variants of `.git`, the apt cache through `bindfs`, and `exit`
//!   back to the root shell.
//! - `root-password`: sets a root password on a passwordless disk with a
//!   provision, then logs in with the right password, a wrong one, and none.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::devices::{clone_disk, required_env, work_dir};

const PASSWORD: &str = "vibe-check-pw";
const PROVISION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const RUN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Commands run as the vm-user through `--script`. Every check prints
/// `VIBE_CHECK_OK <name>` or `VIBE_CHECK_FAIL <name>`; the script fails when
/// any check fails.
const VM_USER_CHECK_SCRIPT: &str = r#"#!/bin/bash
set -u
fail=0
check() {
    if eval "$2" >/dev/null 2>&1; then
        echo "VIBE_CHECK_OK $1"
    else
        echo "VIBE_CHECK_FAIL $1"
        fail=1
    fi
}
check uid '[ "$(id -un)" = dev ] && [ "$(id -u)" != 0 ]'
check cwd '[ "$PWD" = "$HOME/project" ]'
check project-write 'echo from-dev > from-dev.txt && [ "$(cat from-dev.txt)" = from-dev ] && mkdir -p sub && rmdir sub'
check mount-write 'echo from-dev > "$HOME/data/from-dev.txt" && [ "$(cat "$HOME/data/existing.txt")" = existing ]'
check cache-write '[ -d "$HOME/.local/share/mise" ] && touch "$HOME/.local/share/mise/.vibe-check" && rm "$HOME/.local/share/mise/.vibe-check"'
check read-only-mount '[ "$(cat "$HOME/ro/existing.txt")" = existing ] && ! touch "$HOME/ro/new.txt"'
check vibe-masked '[ -z "$(ls -A .vibe)" ] && ! touch .vibe/new'
check env-not-masked '[ "$(cat .env)" = SECRET=from-host ]'
check git-read-only '[ -n "$(cat .git/HEAD)" ] && ! touch .git/new && ! touch .GIT/new && ! touch .Git/new'
check staging '! ls /mnt/root'
check umount '! umount "$HOME/project" && ! umount -l .git'
check apt-cache-bindfs 'findmnt -n -o FSTYPE /var/cache/apt | grep -q fuse && [ -r /var/cache/apt ] && ! touch /var/cache/apt/new'
exit "$fail"
"#;

const VM_USER_CHECKS: &[&str] = &[
    "uid",
    "cwd",
    "project-write",
    "mount-write",
    "cache-write",
    "read-only-mount",
    "vibe-masked",
    "env-not-masked",
    "git-read-only",
    "staging",
    "umount",
    "apt-cache-bindfs",
];

pub(super) fn run_vm_user() -> Result<(), Box<dyn std::error::Error>> {
    with_work_dir("vm-user", |work| {
        let image = provision(work, "t-vmcheck-dev", &["--vm-user", "dev"])?;
        check_missing_user(work, &image)?;
        check_session(work, &image)
    })
}

pub(super) fn run_root_password() -> Result<(), Box<dyn std::error::Error>> {
    with_work_dir("root-password", |work| {
        let image = provision(work, "t-vmcheck-root", &[])?;
        let project = project_dir(work)?;
        let poweroff = ["--send", "systemctl poweroff"];

        let right = run_vibe(
            work,
            &project,
            &disk_args(work, &image, &[&["--root-password", PASSWORD], &poweroff])?,
            RUN_TIMEOUT,
        )?;
        expect_success("right password", &right)?;

        let wrong = run_vibe(
            work,
            &project,
            &disk_args(work, &image, &[&["--root-password", "wrong-pw"], &poweroff])?,
            RUN_TIMEOUT,
        )?;
        expect_failure("wrong password", &wrong, "[login]")?;
        if wrong.text.contains("wrong-pw") || wrong.text.contains(PASSWORD) {
            return Err("the output of the wrong password run contains a password".into());
        }

        let none = run_vibe(
            work,
            &project,
            &disk_args(work, &image, &[&poweroff])?,
            RUN_TIMEOUT,
        )?;
        expect_failure("no password", &none, "--root-password")?;
        eprintln!("root-password: right, wrong, and missing passwords behave as expected");
        Ok(())
    })
}

fn with_work_dir(
    name: &str,
    check: impl FnOnce(&Path) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let work = work_dir(name)?;
    let result = check(&work);
    if result.is_ok() {
        let _ = fs::remove_dir_all(&work);
    } else {
        eprintln!("{name}: leaving {}", work.display());
    }
    result
}

/// Provisions `image` from a clone of `VIBE_VM_CHECK_DISK` into `work` with
/// the root password set, and returns the published raw.
fn provision(
    work: &Path,
    image: &str,
    extra: &[&str],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let source = required_env("VIBE_VM_CHECK_DISK")?;
    if !source.is_file() {
        return Err(format!("VIBE_VM_CHECK_DISK is not a file: {}", source.display()).into());
    }
    let base = work.join("base.raw");
    clone_disk(&source, &base)?;
    let base_arg = base.to_string_lossy().into_owned();
    let mut args = vec![
        "provision",
        "--base",
        &base_arg,
        "--image",
        image,
        "--root-password",
        PASSWORD,
    ];
    args.extend_from_slice(extra);
    let output = run_vibe(work, work, &with_check_args(&args), PROVISION_TIMEOUT)?;
    expect_success("provision", &output)?;
    let raw = work.join(format!("{image}.raw"));
    if !raw.is_file() {
        return Err(format!("provision did not publish {}", raw.display()).into());
    }
    let _ = fs::remove_file(&base);
    Ok(raw)
}

fn check_missing_user(work: &Path, image: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let project = project_dir(work)?;
    let output = run_vibe(
        work,
        &project,
        &disk_args(
            work,
            image,
            &[
                &["--vm-user", "nobody2", "--root-password", PASSWORD],
                &["--send", "systemctl poweroff"],
            ],
        )?,
        RUN_TIMEOUT,
    )?;
    expect_failure("missing vm-user", &output, "nobody2")?;
    eprintln!("vm-user: a missing user fails before the session starts");
    Ok(())
}

fn check_session(work: &Path, image: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let project = project_dir(work)?;
    let data = work.join("data");
    let ro = work.join("ro");
    for dir in [&data, &ro] {
        fs::create_dir_all(dir)?;
        fs::write(dir.join("existing.txt"), "existing\n")?;
    }
    let script = work.join("check-vm-user.sh");
    fs::write(&script, VM_USER_CHECK_SCRIPT)?;
    let data_mount = format!("{}:data:read-write", data.display());
    let ro_mount = format!("{}:ro:read-only", ro.display());
    let script_arg = script.to_string_lossy().into_owned();

    let output = run_vibe(
        work,
        &project,
        &disk_args(
            work,
            image,
            &[
                &["--vm-user", "dev", "--root-password", PASSWORD],
                &["--mount", &data_mount, "--mount", &ro_mount],
                &["--script", &script_arg],
                // `exit` leaves the vm-user's shell; the echo then runs in
                // the root shell. The marker is assembled by the shell so the
                // echoed command line does not match it.
                &["--send", "exit"],
                &["--send", "echo VIBE_BACK_IN_$(id -un)_SHELL"],
                &["--expect", "VIBE_BACK_IN_root_SHELL"],
                &[
                    "--send",
                    "apt-cache gencaches && echo VIBE_ROOT_APT_$((40 + 2))",
                ],
                &["--expect", "VIBE_ROOT_APT_42"],
                &["--send", "systemctl poweroff"],
            ],
        )?,
        RUN_TIMEOUT,
    )?;
    for name in VM_USER_CHECKS {
        if !output.text.contains(&format!("VIBE_CHECK_OK {name}")) {
            return Err(format!(
                "vm-user check '{name}' did not pass; output:\n{}",
                output.text
            )
            .into());
        }
    }
    expect_success("vm-user session", &output)?;

    if fs::read_to_string(project.join("from-dev.txt"))? != "from-dev\n"
        || fs::read_to_string(data.join("from-dev.txt"))? != "from-dev\n"
    {
        return Err("files written by the vm-user are missing on the host".into());
    }
    if ro.join("new.txt").exists() || project.join(".git/new").exists() {
        return Err("the vm-user wrote into a read-only share".into());
    }
    eprintln!(
        "vm-user: all {} session checks passed",
        VM_USER_CHECKS.len()
    );
    Ok(())
}

/// A project directory with a `.env`, a `.vibe` directory, and a `.git`
/// directory.
fn project_dir(work: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let project = work.join("project");
    if !project.is_dir() {
        fs::create_dir_all(project.join(".vibe"))?;
        fs::write(project.join(".vibe/state"), "host state\n")?;
        fs::create_dir_all(project.join(".git"))?;
        fs::write(project.join(".git/HEAD"), "ref: refs/heads/main\n")?;
        fs::write(project.join(".env"), "SECRET=from-host\n")?;
    }
    Ok(project)
}

/// Run arguments for a fresh copy of `image`.
fn disk_args(
    work: &Path,
    image: &Path,
    groups: &[&[&str]],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let disk = work.join("run.raw");
    let _ = fs::remove_file(&disk);
    clone_disk(image, &disk)?;
    let disk = disk.to_string_lossy().into_owned();
    let mut args = Vec::new();
    for group in groups {
        args.extend_from_slice(group);
    }
    args.push(disk.as_str());
    Ok(with_check_args(&args))
}

fn with_check_args(args: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    if let Ok(extra) = std::env::var("VIBE_VM_CHECK_ARGS") {
        // Options go before `provision`'s own options and before any run
        // option; both sides of `provision` accept the shared options.
        let extra: Vec<String> = extra.split_whitespace().map(str::to_string).collect();
        all.splice(0..0, extra);
    }
    all
}

struct VibeOutput {
    success: bool,
    text: String,
}

fn expect_success(what: &str, output: &VibeOutput) -> Result<(), Box<dyn std::error::Error>> {
    if output.success {
        Ok(())
    } else {
        Err(format!("{what} failed; output:\n{}", output.text).into())
    }
}

fn expect_failure(
    what: &str,
    output: &VibeOutput,
    needle: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if output.success {
        return Err(format!("{what} unexpectedly succeeded; output:\n{}", output.text).into());
    }
    if !output.text.contains(needle) {
        return Err(format!("{what} failed without '{needle}'; output:\n{}", output.text).into());
    }
    Ok(())
}

/// Runs the CLI in `cwd` with stdin from /dev/null, with `work` as the
/// preferred image directory, and kills it after `timeout`.
fn run_vibe(
    work: &Path,
    cwd: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<VibeOutput, Box<dyn std::error::Error>> {
    eprintln!("running vibe {}", args.join(" "));
    let mut child = Command::new(std::env::current_exe()?)
        .args(args)
        .current_dir(cwd)
        .env_remove("VIBE_VM_CHECK")
        .env_remove("VIBE_ROOT_PASSWORD")
        .env("VIBE_EXTRA_IMAGE_DIR", work)
        .env("VIBE_PREFERRED_IMAGE_DIR", work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_reader = thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stdout.read_to_end(&mut text);
        text
    });
    let err_reader = thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        text
    });

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(200));
    };
    let mut text = String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned();
    text.push_str(&String::from_utf8_lossy(
        &err_reader.join().unwrap_or_default(),
    ));
    let Some(status) = status else {
        return Err(format!(
            "vibe did not finish within {}s; output:\n{text}",
            timeout.as_secs()
        )
        .into());
    };
    Ok(VibeOutput {
        success: status.success(),
        text,
    })
}
