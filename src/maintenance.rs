//! Run the Alpine maintenance VM against one Debian work copy.
//!
//! Host checks run before any clone or seed is created. A well-formed
//! `PREP-RESULT fail` does not request a stop: the guest powers itself off.
//! Only a protocol or step-order error returns [`FrameDecision::Fail`].
//! Provision calls [`process_disk`] from `ensure_bootable`. The run path does not.

use std::{
    env, fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    alpine::ALPINE_ROOT_FS_UUID,
    cache::{self, FileLock, LockError},
    control::{Frame, FrameReader, Status, new_nonce},
    gpt::{self, FsKind, FsLabel, GptError, Layout},
    image_probe::{self, ProbeFacts},
    seed::{self, Manifest},
    vm_config::{DiskSpec, SerialRole, VmSpec, output_serial},
    vm_lifecycle::{FrameDecision, HeadlessDeadlines, run_headless},
};

const LOGS_TO_KEEP: usize = 5;
const MAINT_CPU: usize = 2;
const MAINT_RAM_BYTES: u64 = 1024 * 1024 * 1024;

pub(crate) const MAINT_STEPS: &[&str] = &[
    "seed", "self", "target", "fsck", "inspect", "apply", "verify", "unmount",
];

pub(crate) fn step_code(name: &str) -> Option<u32> {
    Some(match name {
        "ready" => 1,
        "seed" => 2,
        "self" => 3,
        "target" => 4,
        "fsck" => 5,
        "inspect" => 6,
        "apply" => 7,
        "verify" => 8,
        "unmount" => 9,
        "result" => 10,
        _ => return None,
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StepState {
    ready: bool,
    steps: Vec<String>,
    finished: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StepEvent {
    Continue,
    Succeeded,
    GuestFailed { code: u32, step: String },
    Protocol(String),
}

/// Accept one frame. Step order is `PREP-READY`, then [`MAINT_STEPS`], then
/// `PREP-RESULT`. A guest failure's completed steps must be the prefix of that step.
pub(crate) fn observe_frame(state: &mut StepState, frame: &Frame) -> StepEvent {
    if state.finished {
        return StepEvent::Protocol("frame after PREP-RESULT".into());
    }
    match frame {
        Frame::PrepReady => {
            if state.ready || !state.steps.is_empty() {
                return StepEvent::Protocol("unexpected PREP-READY".into());
            }
            state.ready = true;
            StepEvent::Continue
        }
        Frame::PrepStep { name } => {
            if !state.ready {
                return StepEvent::Protocol("PREP-STEP before PREP-READY".into());
            }
            let expected = MAINT_STEPS.get(state.steps.len()).copied();
            if expected != Some(name.as_str()) {
                return StepEvent::Protocol(format!("unexpected PREP-STEP {name}"));
            }
            state.steps.push(name.clone());
            StepEvent::Continue
        }
        Frame::PrepResult {
            status,
            code,
            reason,
        } => {
            if !state.ready {
                return StepEvent::Protocol("PREP-RESULT before PREP-READY".into());
            }
            state.finished = true;
            match status {
                Status::Ok => {
                    if state
                        .steps
                        .iter()
                        .map(String::as_str)
                        .ne(MAINT_STEPS.iter().copied())
                    {
                        return StepEvent::Protocol("PREP-RESULT ok before every step".into());
                    }
                    if *code != 0 || reason != "done" {
                        return StepEvent::Protocol("PREP-RESULT ok is not code 0 done".into());
                    }
                    StepEvent::Succeeded
                }
                Status::Fail => {
                    let Some(expected_code) = step_code(reason) else {
                        return StepEvent::Protocol(format!("unknown failure step {reason}"));
                    };
                    if *code != expected_code {
                        return StepEvent::Protocol(format!(
                            "failure code {code} does not match step {reason}"
                        ));
                    }
                    let prefix: &[&str] = if reason == "ready" {
                        &[]
                    } else {
                        let Some(index) = MAINT_STEPS.iter().position(|step| *step == reason)
                        else {
                            return StepEvent::Protocol(format!("unknown failure step {reason}"));
                        };
                        &MAINT_STEPS[..index]
                    };
                    if state
                        .steps
                        .iter()
                        .map(String::as_str)
                        .ne(prefix.iter().copied())
                    {
                        return StepEvent::Protocol(format!(
                            "completed steps are not the prefix of {reason}"
                        ));
                    }
                    StepEvent::GuestFailed {
                        code: *code,
                        step: reason.clone(),
                    }
                }
            }
        }
    }
}

pub(crate) fn decision_for(event: &StepEvent) -> FrameDecision {
    match event {
        StepEvent::Protocol(message) => FrameDecision::Fail(message.clone()),
        StepEvent::Continue | StepEvent::Succeeded | StepEvent::GuestFailed { .. } => {
            FrameDecision::Continue
        }
    }
}

/// Reject a label or ext4 UUID that would make Alpine mount the wrong disk.
pub(crate) fn label_block_reason(labels: &[FsLabel]) -> Option<String> {
    for label in labels {
        if label.kind == FsKind::Ext4
            && !label.uuid.is_empty()
            && label.uuid.eq_ignore_ascii_case(ALPINE_ROOT_FS_UUID)
        {
            return Some(format!(
                "ext4 UUID {} matches the Alpine root filesystem. Treat this disk as unsupported.",
                label.uuid
            ));
        }
        let folded = label.label.to_ascii_lowercase();
        if folded == "/" || folded == "cidata" {
            return Some(format!(
                "filesystem label '{}' conflicts with the Alpine maintenance image. Change it with tune2fs -L, or treat this disk as unsupported.",
                label.label
            ));
        }
    }
    None
}

#[derive(Debug)]
pub(crate) struct PrepareError {
    pub stage: String,
    pub last_step: Option<String>,
    pub detail: String,
    pub log: Option<PathBuf>,
    /// Maintenance work directory left behind by `VIBE_KEEP_FAILED_WORK=1`.
    pub kept_dir: Option<PathBuf>,
}

/// Stage of the host re-check that runs after the maintenance VM stopped.
/// Reported with the `[verify]` prefix; every other stage is `[prepare]`.
pub(crate) const HOST_VERIFY_STAGE: &str = "host-verify";

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let phase = if self.stage == HOST_VERIFY_STAGE {
            "verify"
        } else {
            "prepare"
        };
        if let Some(step) = &self.last_step {
            write!(
                f,
                "[{phase}] {} (last PREP-STEP: {step}): {}",
                self.stage, self.detail
            )?;
        } else {
            write!(f, "[{phase}] {}: {}", self.stage, self.detail)?;
        }
        if let Some(log) = &self.log {
            write!(f, "\nlog: {}", log.display())?;
        }
        if let Some(dir) = &self.kept_dir {
            write!(
                f,
                "\nVIBE_KEEP_FAILED_WORK=1: kept the maintenance work directory at {}",
                dir.display()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for PrepareError {}

impl PrepareError {
    fn new(stage: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            last_step: None,
            detail: detail.into(),
            log: None,
            kept_dir: None,
        }
    }
}

struct PreparedDisk {
    _lock: FileLock,
    before: ProbeFacts,
    layout: Layout,
    disk_bytes: u64,
}

/// Edit `disk` from the Alpine maintenance VM, then re-check it.
/// `alpine_raw` is the cached read-only input. Work files live under `maint/`
/// next to `alpine_raw`, or under `fallback_root/maint/` when that directory
/// cannot be created. The console log is published to `log_dir/logs/`.
pub(crate) fn process_disk(
    disk: &Path,
    alpine_raw: &Path,
    fallback_root: &Path,
    log_dir: &Path,
) -> Result<(), PrepareError> {
    let prepared = prepare_target(disk)?;
    let work = create_work_dir(alpine_raw, fallback_root)?;
    let keep_failed = env::var("VIBE_KEEP_FAILED_WORK").ok().as_deref() == Some("1");
    let mut guard = WorkGuard {
        dir: work.clone(),
        keep: false,
    };
    let log_inside = work.join("hvc0.log");
    let result = run_maintenance(&work, disk, alpine_raw, &prepared, &log_inside);
    let published = if log_inside.exists() {
        let header = log_header(disk, alpine_raw, &work);
        match publish_log(&log_inside, log_dir, &header) {
            Ok(path) => Some(path),
            Err(err) => {
                guard.keep = keep_failed;
                let base = match result {
                    Err(error) => error,
                    Ok(()) => PrepareError::new("log", "failed to publish the maintenance log"),
                };
                let mut error = attach_log(base, Some(err));
                error.kept_dir = keep_failed.then(|| work.clone());
                return Err(error);
            }
        }
    } else {
        None
    };
    match result {
        Ok(()) => image_probe::verify_processed(disk, &prepared.before).map_err(|err| {
            guard.keep = keep_failed;
            PrepareError {
                stage: HOST_VERIFY_STAGE.into(),
                last_step: Some("unmount".into()),
                detail: err.to_string(),
                log: published,
                kept_dir: keep_failed.then(|| work.clone()),
            }
        }),
        Err(mut error) => {
            guard.keep = keep_failed;
            error.log = published.or(error.log);
            error.kept_dir = keep_failed.then(|| work.clone());
            Err(error)
        }
    }
}

/// Create `maint/<pid>-<nonce>/` next to `alpine_raw` so the clone stays on
/// one file system, falling back to `fallback_root/maint/`. Only the `maint/`
/// root that is used is swept.
fn create_work_dir(alpine_raw: &Path, fallback_root: &Path) -> Result<PathBuf, PrepareError> {
    let fallback = fallback_root.join("maint");
    let primary = alpine_raw
        .parent()
        .map(|dir| dir.join("maint"))
        .unwrap_or_else(|| fallback.clone());
    let (root, work) = match cache::private_dir(&primary, "") {
        Ok(work) => (primary, work),
        Err(err) if primary != fallback => {
            println!(
                "Cannot create the maintenance work directory under {}: {err}. Using {} instead.",
                primary.display(),
                fallback.display()
            );
            let work = cache::private_dir(&fallback, "").map_err(io_prepare)?;
            (fallback, work)
        }
        Err(err) => return Err(io_prepare(err)),
    };
    if let Err(err) = cache::sweep_stale(&root) {
        let _ = fs::remove_dir_all(&work);
        return Err(io_prepare(err));
    }
    Ok(work)
}

/// Host-written lines at the top of a published log. The maintenance files
/// may live in different image directories, so every path is absolute.
pub(crate) fn log_header(disk: &Path, alpine_raw: &Path, work: &Path) -> String {
    let absolute = |path: &Path| {
        std::path::absolute(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string()
    };
    format!(
        "# Vibe maintenance log\n\
# disk: {}\n\
# alpine source: {}\n\
# alpine clone: {}\n\
# seed iso: {}\n\
# work directory: {} (removed afterwards; kept after a failure when VIBE_KEEP_FAILED_WORK=1)\n\
# hvc0 console output follows\n",
        absolute(disk),
        absolute(alpine_raw),
        absolute(&work.join("alpine.raw")),
        absolute(&work.join("seed.iso")),
        absolute(work),
    )
}

struct WorkGuard {
    dir: PathBuf,
    keep: bool,
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn run_maintenance(
    work: &Path,
    disk: &Path,
    alpine_raw: &Path,
    prepared: &PreparedDisk,
    log_path: &Path,
) -> Result<(), PrepareError> {
    let nonce = new_nonce().map_err(io_prepare)?;
    let alpine_clone = work.join("alpine.raw");
    cache::clone_file(alpine_raw, &alpine_clone).map_err(io_prepare)?;
    fs::set_permissions(&alpine_clone, fs::Permissions::from_mode(0o644)).map_err(io_prepare)?;

    let mut root_partuuid = prepared.layout.root.unique_guid.clone();
    if let Ok(override_id) = env::var("VIBE_MAINT_TEST_PARTUUID")
        && !override_id.is_empty()
    {
        root_partuuid = override_id;
    }
    let manifest = Manifest {
        format: 1,
        nonce: nonce.clone(),
        alpine_root_uuid: ALPINE_ROOT_FS_UUID.to_string(),
        disk_bytes: prepared.disk_bytes,
        root_part: prepared.layout.root.index,
        root_partuuid,
        root_type: prepared.layout.root.type_guid.clone(),
        esp_part: prepared.layout.esp.index,
        policy: "minimal".to_string(),
    };
    seed::build_seed(work, &manifest).map_err(|err| PrepareError::new("seed", err.to_string()))?;

    let spec = VmSpec {
        disks: vec![
            DiskSpec {
                path: alpine_clone,
                read_only: false,
            },
            DiskSpec {
                path: disk.to_path_buf(),
                read_only: false,
            },
            DiskSpec {
                path: work.join("seed.iso"),
                read_only: true,
            },
        ],
        network: None,
        share_devices: Vec::new(),
        serial_ports: vec![
            output_serial(SerialRole::Log),
            output_serial(SerialRole::Control),
        ],
        efi_vars: work.join("efivars"),
        cpu_count: MAINT_CPU,
        ram_bytes: MAINT_RAM_BYTES,
    };

    let mut watch = Watch::default();
    let outcome = run_headless(
        spec,
        log_path,
        FrameReader::new(nonce),
        HeadlessDeadlines::maintenance(),
        |frame| {
            let event = observe_frame(&mut watch.state, frame);
            let decision = decision_for(&event);
            match event {
                StepEvent::Succeeded => watch.success = true,
                StepEvent::GuestFailed { code, step } => watch.guest_fail = Some((code, step)),
                StepEvent::Continue | StepEvent::Protocol(_) => {}
            }
            decision
        },
    )
    .map_err(|err| PrepareError::new("boot", err.to_string()))?;

    if let Some(error) = outcome.error {
        let stage = if error.contains("PREP-READY") {
            "ready"
        } else if error.contains("PREP-RESULT") || error.contains("to stop") {
            "timeout"
        } else {
            "protocol"
        };
        return Err(PrepareError {
            stage: stage.into(),
            last_step: watch.state.steps.last().cloned(),
            detail: error,
            log: None,
            kept_dir: None,
        });
    }
    if !outcome.stopped_normally {
        return Err(PrepareError {
            stage: "timeout".into(),
            last_step: watch.state.steps.last().cloned(),
            detail: "maintenance VM did not stop normally".into(),
            log: None,
            kept_dir: None,
        });
    }
    if let Some((code, step)) = watch.guest_fail {
        let last_step = previous_step(&step);
        return Err(PrepareError {
            stage: step,
            last_step,
            detail: format!("guest reported failure code {code}"),
            log: None,
            kept_dir: None,
        });
    }
    if !watch.success {
        return Err(PrepareError {
            stage: "result".into(),
            last_step: watch.state.steps.last().cloned(),
            detail: "maintenance VM stopped without PREP-RESULT ok".into(),
            log: None,
            kept_dir: None,
        });
    }
    Ok(())
}

#[derive(Default)]
struct Watch {
    state: StepState,
    success: bool,
    guest_fail: Option<(u32, String)>,
}

fn previous_step(failed: &str) -> Option<String> {
    let index = MAINT_STEPS.iter().position(|step| *step == failed)?;
    if index == 0 {
        None
    } else {
        Some(MAINT_STEPS[index - 1].to_string())
    }
}

fn prepare_target(disk: &Path) -> Result<PreparedDisk, PrepareError> {
    let meta = fs::symlink_metadata(disk).map_err(|err| {
        PrepareError::new("safety", format!("cannot stat {}: {err}", disk.display()))
    })?;
    if !meta.file_type().is_file() {
        return Err(PrepareError::new(
            "safety",
            format!("{} is not a regular file", disk.display()),
        ));
    }
    if meta.permissions().mode() & 0o222 == 0 {
        return Err(PrepareError::new(
            "safety",
            format!("{} is not writable", disk.display()),
        ));
    }
    let lock = match FileLock::try_acquire_existing(disk) {
        Ok(lock) => lock,
        Err(LockError::WouldBlock) => {
            return Err(PrepareError::new(
                "safety",
                format!("disk is in use by another Vibe process: {}", disk.display()),
            ));
        }
        Err(LockError::Io(err)) if err.kind() == io::ErrorKind::PermissionDenied => {
            return Err(PrepareError::new(
                "safety",
                format!("{} is not writable", disk.display()),
            ));
        }
        Err(err) => return Err(PrepareError::new("safety", err.to_string())),
    };

    let mut file = fs::File::open(disk).map_err(io_prepare)?;
    let disk_bytes = meta.len();
    let gpt = gpt::read_gpt(&mut file).map_err(gpt_prepare)?;
    let layout = gpt::select_layout(&gpt, disk_bytes).map_err(gpt_prepare)?;
    let labels = gpt::scan_labels(&mut file, &gpt).map_err(gpt_prepare)?;
    if let Some(reason) = label_block_reason(&labels) {
        return Err(PrepareError::new("safety", reason));
    }
    let (_class, before) = image_probe::probe_disk(disk);
    Ok(PreparedDisk {
        _lock: lock,
        before,
        layout,
        disk_bytes,
    })
}

fn gpt_prepare(err: GptError) -> PrepareError {
    PrepareError::new("safety", err.to_string())
}

fn io_prepare(err: io::Error) -> PrepareError {
    PrepareError::new("prepare", err.to_string())
}

fn attach_log(mut error: PrepareError, log_error: Option<io::Error>) -> PrepareError {
    if let Some(err) = log_error {
        error.detail = format!("{}; log move failed: {err}", error.detail);
    }
    error
}

/// Write `header` and then the bytes of `src` to `log_dir/logs/`, and remove `src`.
pub(crate) fn publish_log(src: &Path, log_dir: &Path, header: &str) -> io::Result<PathBuf> {
    let logs = log_dir.join("logs");
    fs::create_dir_all(&logs)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = logs.join(format!("maint-{stamp}-{}.log", std::process::id()));
    let written = (|| -> io::Result<()> {
        let mut out = fs::File::create(&dest)?;
        out.write_all(header.as_bytes())?;
        io::copy(&mut fs::File::open(src)?, &mut out)?;
        out.sync_all()
    })();
    if let Err(err) = written {
        let _ = fs::remove_file(&dest);
        return Err(err);
    }
    fs::remove_file(src)?;
    retain_newest_logs(&logs, LOGS_TO_KEEP)?;
    Ok(dest)
}

pub(crate) fn retain_newest_logs(dir: &Path, keep: usize) -> io::Result<()> {
    let mut logs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("maint-") && name.ends_with(".log")) {
            continue;
        }
        let modified = entry.metadata()?.modified().unwrap_or(UNIX_EPOCH);
        logs.push((modified, entry.path()));
    }
    logs.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for (_, path) in logs.into_iter().skip(keep) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{Frame, Status};
    use std::time::Duration;

    fn ready() -> Frame {
        Frame::PrepReady
    }
    fn step(name: &str) -> Frame {
        Frame::PrepStep { name: name.into() }
    }
    fn result(status: Status, code: u32, reason: &str) -> Frame {
        Frame::PrepResult {
            status,
            code,
            reason: reason.into(),
        }
    }

    #[test]
    fn step_order_accepts_the_full_success_sequence() {
        let mut state = StepState::default();
        assert_eq!(observe_frame(&mut state, &ready()), StepEvent::Continue);
        for name in MAINT_STEPS {
            assert_eq!(observe_frame(&mut state, &step(name)), StepEvent::Continue);
        }
        assert_eq!(
            observe_frame(&mut state, &result(Status::Ok, 0, "done")),
            StepEvent::Succeeded
        );
    }

    #[test]
    fn guest_failure_must_be_a_prefix_and_is_not_a_protocol_error() {
        let mut state = StepState::default();
        observe_frame(&mut state, &ready());
        observe_frame(&mut state, &step("seed"));
        observe_frame(&mut state, &step("self"));
        observe_frame(&mut state, &step("target"));
        let event = observe_frame(&mut state, &result(Status::Fail, 5, "fsck"));
        assert_eq!(
            event,
            StepEvent::GuestFailed {
                code: 5,
                step: "fsck".into()
            }
        );
        assert!(matches!(decision_for(&event), FrameDecision::Continue));
    }

    #[test]
    fn out_of_order_steps_are_protocol_errors() {
        let mut state = StepState::default();
        assert!(matches!(
            observe_frame(&mut state, &step("seed")),
            StepEvent::Protocol(_)
        ));
        let mut state = StepState::default();
        observe_frame(&mut state, &ready());
        observe_frame(&mut state, &step("seed"));
        let event = observe_frame(&mut state, &step("fsck"));
        assert!(matches!(event, StepEvent::Protocol(_)));
        assert!(matches!(decision_for(&event), FrameDecision::Fail(_)));
        let mut state = StepState::default();
        observe_frame(&mut state, &ready());
        let event = observe_frame(&mut state, &result(Status::Ok, 0, "done"));
        assert!(matches!(event, StepEvent::Protocol(_)));
        let mut state = StepState::default();
        observe_frame(&mut state, &ready());
        let event = observe_frame(&mut state, &result(Status::Fail, 1, "fsck"));
        assert!(matches!(event, StepEvent::Protocol(_)));
    }

    #[test]
    fn label_conflict_matches_reserved_names_and_the_alpine_uuid() {
        let labels = vec![
            FsLabel {
                part: Some(1),
                kind: FsKind::Ext4,
                label: "/".into(),
                uuid: "11111111-1111-1111-1111-111111111111".into(),
            },
            FsLabel {
                part: Some(2),
                kind: FsKind::Iso9660,
                label: "CIDATA".into(),
                uuid: String::new(),
            },
        ];
        let reason = label_block_reason(&labels).unwrap();
        assert!(reason.contains("tune2fs -L"), "{reason}");
        let alpine = vec![FsLabel {
            part: Some(2),
            kind: FsKind::Ext4,
            label: "root".into(),
            uuid: ALPINE_ROOT_FS_UUID.to_ascii_uppercase(),
        }];
        assert!(label_block_reason(&alpine).unwrap().contains("UUID"));
        let ok = vec![FsLabel {
            part: Some(15),
            kind: FsKind::Fat,
            label: "EFI".into(),
            uuid: String::new(),
        }];
        assert!(label_block_reason(&ok).is_none());
    }

    #[test]
    fn prepare_errors_use_prepare_or_verify_prefixes_and_name_kept_paths() {
        let guest = PrepareError {
            stage: "verify".into(),
            last_step: Some("apply".into()),
            detail: "guest reported failure code 8".into(),
            log: Some(PathBuf::from("/cache/logs/maint-1-2.log")),
            kept_dir: None,
        };
        let text = guest.to_string();
        assert!(
            text.starts_with("[prepare] verify (last PREP-STEP: apply): guest reported"),
            "{text}"
        );
        assert!(text.contains("\nlog: /cache/logs/maint-1-2.log"));
        assert!(!text.contains("VIBE_KEEP_FAILED_WORK"));

        let host = PrepareError {
            stage: HOST_VERIFY_STAGE.into(),
            last_step: Some("unmount".into()),
            detail: "machine-id is not empty".into(),
            log: None,
            kept_dir: Some(PathBuf::from("/cache/maint/1-abc")),
        };
        let text = host.to_string();
        assert!(text.starts_with("[verify] host-verify (last PREP-STEP: unmount):"));
        assert!(text.contains(
            "VIBE_KEEP_FAILED_WORK=1: kept the maintenance work directory at /cache/maint/1-abc"
        ));

        let safety = PrepareError::new("safety", "not a regular file");
        assert_eq!(safety.to_string(), "[prepare] safety: not a regular file");
    }

    #[test]
    fn log_retention_keeps_the_five_newest() {
        let dir = std::env::temp_dir().join(format!("vibe-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for index in 0..7 {
            let path = dir.join(format!("maint-{index}.log"));
            fs::write(&path, format!("log {index}")).unwrap();
            let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000 + index);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        fs::write(dir.join("notes.txt"), b"keep").unwrap();
        retain_newest_logs(&dir, 5).unwrap();
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert!(names.contains(&"notes.txt".to_string()));
        let logs: Vec<_> = names.iter().filter(|name| name.ends_with(".log")).collect();
        assert_eq!(logs.len(), 5, "{names:?}");
        assert!(!names.iter().any(|name| name == "maint-0.log"));
        assert!(!names.iter().any(|name| name == "maint-1.log"));
        let _ = fs::remove_dir_all(&dir);
    }
}
