//! Classify a disk image and re-check it after maintenance.
//!
//! `probe_disk` never panics and never returns a hard error: a disk that is
//! not a Debian GPT image is `Unsupported`, and anything the bounded reader
//! cannot decide is `Unknown`. Shadow bytes are parsed for the root password
//! field and then zeroed; they are not stored on `ProbeFacts`.
//!
//! `/etc/os-release` on current Debian is one symlink to `../usr/lib/os-release`.
//! That final component is resolved once, lexically, and the target is read
//! only when every earlier component is a real directory. No other path is
//! followed. The run path does not call this module. Provision does, from
//! `ensure_bootable`, and again before it publishes an image.
//!
//! `verify_processed` runs after the maintenance VM stops.

use std::{fs::File, hint::black_box, path::Path};

use crate::{
    ext4ro::{Ext4Error, Ext4Ro, FileRead},
    gpt,
};

const SMALL_FILE_LIMIT: usize = 64 * 1024;
const SHADOW_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImageClass {
    LegacyCompatible,
    NeedsPreparation,
    Unknown(String),
    Unsupported(String),
}

impl std::fmt::Display for ImageClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageClass::LegacyCompatible => write!(f, "legacy compatible"),
            ImageClass::NeedsPreparation => write!(f, "needs preparation"),
            ImageClass::Unknown(reason) => write!(f, "unknown: {reason}"),
            ImageClass::Unsupported(reason) => write!(f, "unsupported: {reason}"),
        }
    }
}

/// machine-id as seen on disk. `Missing` and `EmptyFile` classify the same
/// way; `verify_processed` accepts only a zero-byte regular file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum MachineIdFact {
    #[default]
    Missing,
    EmptyFile,
    Valid,
    Uninitialized,
    Other,
    Symlink(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum DbusState {
    #[default]
    Missing,
    EmptyFile,
    LinkToEtc,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PartSnap {
    pub index: u32,
    pub type_guid: String,
    pub unique_guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

/// Facts used by [`classify`]. Shadow plaintext is intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ProbeFacts {
    pub is_debian: Option<bool>,
    pub debian_version: Option<String>,
    pub machine_id: MachineIdFact,
    pub root_line_count: Option<u32>,
    pub root_password_empty: Option<bool>,
    pub dbus: DbusState,
    pub layout: Option<Vec<PartSnap>>,
    pub gpt_detail: String,
    /// Set when a read could not be classified. Checked before the table.
    pub fault: Option<String>,
    /// Set for disks that stay on the legacy path (`no GPT root`).
    pub unsupported: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VerifyError {
    LayoutChanged,
    MachineIdSymlink,
    MachineIdNotEmpty,
    RootLineCount,
    RootPasswordNotEmpty,
    DbusMachineId,
    NotLegacy,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            VerifyError::LayoutChanged => "GPT layout changed during preparation",
            VerifyError::MachineIdSymlink => "machine-id is a symbolic link",
            VerifyError::MachineIdNotEmpty => "machine-id is not an empty regular file",
            VerifyError::RootLineCount => "shadow does not contain exactly one root line",
            VerifyError::RootPasswordNotEmpty => "root password field is not empty",
            VerifyError::DbusMachineId => "dbus machine-id is not an accepted value",
            VerifyError::NotLegacy => "image is not legacy compatible after preparation",
        };
        write!(f, "{text}")
    }
}

impl std::error::Error for VerifyError {}

pub(crate) fn probe_disk(path: &Path) -> (ImageClass, ProbeFacts) {
    // Lets tests confirm which paths probe a disk (running a disk must not).
    // Unset in normal use.
    if std::env::var_os("VIBE_TRACE_PROBE").is_some() {
        eprintln!("vibe: probe_disk {}", path.display());
    }
    let mut facts = ProbeFacts::default();
    inspect(path, &mut facts);
    let class = classify(&facts);
    (class, facts)
}

pub(crate) fn classify(facts: &ProbeFacts) -> ImageClass {
    if let Some(reason) = &facts.unsupported {
        return ImageClass::Unsupported(reason.clone());
    }
    if let Some(reason) = &facts.fault {
        return ImageClass::Unknown(reason.clone());
    }
    if facts.is_debian == Some(false) {
        return ImageClass::Unsupported("not Debian".to_string());
    }
    if facts.is_debian != Some(true) {
        return ImageClass::Unknown("cannot read os-release".to_string());
    }
    if matches!(facts.machine_id, MachineIdFact::Symlink(_)) {
        return ImageClass::Unknown("machine-id is a symlink".to_string());
    }
    let bad_id = matches!(
        facts.machine_id,
        MachineIdFact::Uninitialized | MachineIdFact::Other
    );
    if bad_id {
        return ImageClass::NeedsPreparation;
    }
    if !matches!(
        facts.machine_id,
        MachineIdFact::Missing | MachineIdFact::EmptyFile | MachineIdFact::Valid
    ) {
        return ImageClass::Unknown("cannot read machine-id".to_string());
    }
    let count = facts.root_line_count.unwrap_or(0);
    if count == 1 && facts.root_password_empty == Some(true) {
        return ImageClass::LegacyCompatible;
    }
    if count == 1 && facts.root_password_empty == Some(false) {
        return ImageClass::NeedsPreparation;
    }
    ImageClass::Unknown(format!("root account line count is {count}"))
}

pub(crate) fn verify_processed(path: &Path, before: &ProbeFacts) -> Result<(), VerifyError> {
    let (_class, facts) = probe_disk(path);
    if facts.layout != before.layout {
        return Err(VerifyError::LayoutChanged);
    }
    if matches!(facts.machine_id, MachineIdFact::Symlink(_)) {
        return Err(VerifyError::MachineIdSymlink);
    }
    if !matches!(facts.machine_id, MachineIdFact::EmptyFile) {
        return Err(VerifyError::MachineIdNotEmpty);
    }
    if facts.root_line_count != Some(1) {
        return Err(VerifyError::RootLineCount);
    }
    if facts.root_password_empty != Some(true) {
        return Err(VerifyError::RootPasswordNotEmpty);
    }
    if !matches!(
        facts.dbus,
        DbusState::Missing | DbusState::EmptyFile | DbusState::LinkToEtc
    ) {
        return Err(VerifyError::DbusMachineId);
    }
    if classify(&facts) != ImageClass::LegacyCompatible {
        return Err(VerifyError::NotLegacy);
    }
    Ok(())
}

fn inspect(path: &Path, facts: &mut ProbeFacts) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => {
            facts.fault = Some("cannot open disk image".to_string());
            return;
        }
    };
    let disk_bytes = match file.metadata() {
        Ok(meta) => meta.len(),
        Err(_) => {
            facts.fault = Some("cannot open disk image".to_string());
            return;
        }
    };
    let gpt = match gpt::read_gpt(&mut file) {
        Ok(gpt) => gpt,
        Err(err) => {
            facts.gpt_detail = err.to_string();
            facts.unsupported = Some("no GPT root".to_string());
            return;
        }
    };
    facts.layout = Some(snap_layout(&gpt));
    let layout = match gpt::select_layout(&gpt, disk_bytes) {
        Ok(layout) => layout,
        Err(err) => {
            facts.gpt_detail = err.to_string();
            facts.unsupported = Some("no GPT root".to_string());
            return;
        }
    };
    let Some((offset, len)) = partition_span(layout.root.first_lba, layout.root.last_lba) else {
        facts.fault = Some("root partition size overflowed".to_string());
        return;
    };
    // Extent reads use pread, so the cursor left by `read_gpt` does not matter.
    let fs = match Ext4Ro::open(&file, offset, len) {
        Ok(fs) => fs,
        Err(err) => {
            facts.fault = Some(ext4_fault(&err));
            return;
        }
    };
    match read_os_release(&fs) {
        Ok(text) => facts.is_debian = Some(debian_os_release(&text)),
        Err(reason) => note(facts, reason),
    }
    match fs.read_file("/etc/debian_version", SMALL_FILE_LIMIT) {
        Ok(FileRead::Regular(bytes)) => {
            facts.debian_version = Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        Ok(FileRead::Missing) => {}
        Ok(FileRead::Symlink(_)) => note(facts, "/etc/debian_version is a symlink".to_string()),
        Ok(FileRead::Other) => note(
            facts,
            "/etc/debian_version is not a regular file".to_string(),
        ),
        Err(err) => note(facts, format!("cannot read /etc/debian_version: {err}")),
    }
    match fs.read_file("/etc/machine-id", SMALL_FILE_LIMIT) {
        Ok(FileRead::Missing) => facts.machine_id = MachineIdFact::Missing,
        Ok(FileRead::Regular(bytes)) => facts.machine_id = machine_id_of(&bytes),
        Ok(FileRead::Symlink(target)) => facts.machine_id = MachineIdFact::Symlink(target),
        Ok(FileRead::Other) => note(facts, "/etc/machine-id is not a regular file".to_string()),
        Err(err) => note(facts, format!("cannot read /etc/machine-id: {err}")),
    }
    match fs.read_file("/etc/shadow", SHADOW_LIMIT) {
        Ok(FileRead::Missing) => facts.root_line_count = Some(0),
        Ok(FileRead::Regular(mut bytes)) => {
            let (count, empty) = parse_shadow(&bytes);
            bytes.fill(0);
            black_box(&bytes);
            facts.root_line_count = Some(count);
            facts.root_password_empty = empty;
        }
        Ok(FileRead::Symlink(_)) => note(facts, "/etc/shadow is a symlink".to_string()),
        Ok(FileRead::Other) => note(facts, "/etc/shadow is not a regular file".to_string()),
        Err(err) => note(facts, format!("cannot read /etc/shadow: {err}")),
    }
    facts.dbus = read_dbus(&fs);
}

fn note(facts: &mut ProbeFacts, reason: String) {
    if facts.fault.is_none() {
        facts.fault = Some(reason);
    }
}

fn ext4_fault(err: &Ext4Error) -> String {
    if err.is_needs_recovery() {
        "filesystem needs recovery".to_string()
    } else {
        format!("cannot read root filesystem: {err}")
    }
}

fn snap_layout(gpt: &gpt::Gpt) -> Vec<PartSnap> {
    gpt.partitions
        .iter()
        .map(|part| PartSnap {
            index: part.index,
            type_guid: part.type_guid.clone(),
            unique_guid: part.unique_guid.clone(),
            first_lba: part.first_lba,
            last_lba: part.last_lba,
        })
        .collect()
}

fn partition_span(first_lba: u64, last_lba: u64) -> Option<(u64, u64)> {
    let offset = first_lba.checked_mul(512)?;
    let sectors = last_lba.checked_sub(first_lba)?.checked_add(1)?;
    let len = sectors.checked_mul(512)?;
    Some((offset, len))
}

fn read_os_release<R: crate::ext4ro::ReadAt>(fs: &Ext4Ro<R>) -> Result<String, String> {
    match fs.read_file("/etc/os-release", SMALL_FILE_LIMIT) {
        Ok(FileRead::Regular(bytes)) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Ok(FileRead::Symlink(target)) => {
            let path = lexical_join("/etc", &target);
            match fs.read_file(&path, SMALL_FILE_LIMIT) {
                Ok(FileRead::Regular(bytes)) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
                Ok(FileRead::Symlink(_)) => Err("os-release target is a symlink".to_string()),
                Ok(FileRead::Missing) => Err("os-release target is missing".to_string()),
                Ok(FileRead::Other) => Err("os-release target is not a regular file".to_string()),
                Err(err) => Err(format!("cannot read os-release target: {err}")),
            }
        }
        Ok(FileRead::Missing) => Err("os-release is missing".to_string()),
        Ok(FileRead::Other) => Err("os-release is not a regular file".to_string()),
        Err(err) => Err(format!("cannot read os-release: {err}")),
    }
}

fn read_dbus<R: crate::ext4ro::ReadAt>(fs: &Ext4Ro<R>) -> DbusState {
    match fs.read_file("/var/lib/dbus/machine-id", SMALL_FILE_LIMIT) {
        Ok(FileRead::Missing) => DbusState::Missing,
        Ok(FileRead::Regular(bytes)) if bytes.is_empty() => DbusState::EmptyFile,
        Ok(FileRead::Symlink(target)) => {
            if lexical_join("/var/lib/dbus", &target) == "/etc/machine-id" {
                DbusState::LinkToEtc
            } else {
                DbusState::Other
            }
        }
        Ok(FileRead::Regular(_)) | Ok(FileRead::Other) => DbusState::Other,
        Err(_) => DbusState::Other,
    }
}

/// Join `target` onto `dir` and collapse `.` and `..`. Extra `..` stays at `/`.
fn lexical_join(dir: &str, target: &str) -> String {
    let mut stack: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|part| !part.is_empty()).collect()
    };
    for part in target.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            stack.pop();
            continue;
        }
        stack.push(part);
    }
    if stack.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", stack.join("/"))
    }
}

fn debian_os_release(text: &str) -> bool {
    let mut id = None;
    let mut like = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "ID" => id = Some(value),
            "ID_LIKE" => like = Some(value),
            _ => {}
        }
    }
    if id.as_deref() == Some("debian") {
        return true;
    }
    like.as_deref()
        .is_some_and(|like| like.split_whitespace().any(|token| token == "debian"))
}

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn machine_id_of(bytes: &[u8]) -> MachineIdFact {
    if bytes.is_empty() {
        return MachineIdFact::EmptyFile;
    }
    if bytes == b"uninitialized\n" {
        return MachineIdFact::Uninitialized;
    }
    if bytes.len() == 33
        && bytes[32] == b'\n'
        && bytes[..32]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return MachineIdFact::Valid;
    }
    MachineIdFact::Other
}

fn parse_shadow(buf: &[u8]) -> (u32, Option<bool>) {
    let mut count = 0u32;
    let mut empty = false;
    for line in buf.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix(b"root:") else {
            continue;
        };
        count = count.saturating_add(1);
        let field = rest.split(|byte| *byte == b':').next().unwrap_or(b"");
        empty = field.is_empty();
    }
    if count == 1 {
        (count, Some(empty))
    } else {
        (count, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ext4ro::sample_ext4_image;
    use crate::gpt::{ESP_TYPE_GUID, LINUX_ROOT_ARM64_GUID, TestPart, build_gpt};
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    const ROOT_A: &str = "2dee7ac4-57d3-494b-9090-ddc9a1e78576";
    const ROOT_B: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const ESP_ID: &str = "11111111-2222-3333-4444-555555555555";

    struct TempDisk(std::path::PathBuf);
    impl Drop for TempDisk {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn gunzip(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let output = std::process::Command::new("/usr/bin/gzip")
            .arg("-dc")
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{name}");
        output.stdout
    }

    fn wrap(ext4: &[u8], root_unique: &'static str) -> TempDisk {
        let mut ext4 = ext4.to_vec();
        let rem = ext4.len() % 512;
        if rem != 0 {
            ext4.resize(ext4.len() + (512 - rem), 0);
        }
        let root_first = 2048u64;
        let sectors = (ext4.len() / 512) as u64;
        let root_last = root_first + sectors - 1;
        let disk_bytes = (root_last + 1) * 512;
        let parts = [
            TestPart {
                index: 1,
                type_guid: LINUX_ROOT_ARM64_GUID,
                unique_guid: root_unique,
                first_lba: root_first,
                last_lba: root_last,
                name: "root",
            },
            TestPart {
                index: 15,
                type_guid: ESP_TYPE_GUID,
                unique_guid: ESP_ID,
                first_lba: 34,
                last_lba: 2047,
                name: "ESP",
            },
        ];
        let gpt = build_gpt(&parts, disk_bytes, 128);
        let mut image = vec![0u8; disk_bytes as usize];
        let n = gpt.len().min(image.len());
        image[..n].copy_from_slice(&gpt[..n]);
        let off = root_first as usize * 512;
        image[off..off + ext4.len()].copy_from_slice(&ext4);
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vibe-probe-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, image).unwrap();
        TempDisk(path)
    }

    fn legacy_facts() -> ProbeFacts {
        ProbeFacts {
            is_debian: Some(true),
            machine_id: MachineIdFact::EmptyFile,
            root_line_count: Some(1),
            root_password_empty: Some(true),
            layout: Some(Vec::new()),
            ..ProbeFacts::default()
        }
    }

    #[test]
    fn classify_covers_machine_id_and_root_password() {
        let ids = [
            MachineIdFact::EmptyFile,
            MachineIdFact::Missing,
            MachineIdFact::Valid,
            MachineIdFact::Uninitialized,
            MachineIdFact::Other,
        ];
        let roots = [
            (Some(1), Some(true)),
            (Some(1), Some(false)),
            (Some(0), None),
        ];
        for id in ids {
            for (count, empty) in roots {
                let mut facts = legacy_facts();
                facts.machine_id = id.clone();
                facts.root_line_count = count;
                facts.root_password_empty = empty;
                let class = classify(&facts);
                let id_ok = matches!(
                    id,
                    MachineIdFact::EmptyFile | MachineIdFact::Missing | MachineIdFact::Valid
                );
                let expected = if !id_ok {
                    ImageClass::NeedsPreparation
                } else if count == Some(1) && empty == Some(true) {
                    ImageClass::LegacyCompatible
                } else if count == Some(1) && empty == Some(false) {
                    ImageClass::NeedsPreparation
                } else {
                    ImageClass::Unknown(format!(
                        "root account line count is {}",
                        count.unwrap_or(0)
                    ))
                };
                assert_eq!(class, expected, "{id:?} count={count:?} empty={empty:?}");
            }
        }

        let mut two = legacy_facts();
        two.root_line_count = Some(2);
        two.root_password_empty = None;
        assert_eq!(
            classify(&two),
            ImageClass::Unknown("root account line count is 2".into())
        );
        two.machine_id = MachineIdFact::Uninitialized;
        assert_eq!(classify(&two), ImageClass::NeedsPreparation);

        let mut alpine = legacy_facts();
        alpine.is_debian = Some(false);
        alpine.root_password_empty = Some(false);
        assert_eq!(
            classify(&alpine),
            ImageClass::Unsupported("not Debian".into())
        );

        let mut link = legacy_facts();
        link.machine_id = MachineIdFact::Symlink("../x".into());
        assert_eq!(
            classify(&link),
            ImageClass::Unknown("machine-id is a symlink".into())
        );
    }

    #[test]
    fn lexical_symlink_targets() {
        assert_eq!(
            lexical_join("/etc", "../usr/lib/os-release"),
            "/usr/lib/os-release"
        );
        assert_eq!(
            lexical_join("/var/lib/dbus", "../../../etc/machine-id"),
            "/etc/machine-id"
        );
        assert_eq!(
            lexical_join("/var/lib/dbus", "../../../../etc/machine-id"),
            "/etc/machine-id"
        );
        assert_eq!(
            lexical_join("/var/lib/dbus", "/etc/machine-id"),
            "/etc/machine-id"
        );
        assert_eq!(
            lexical_join("/var/lib/dbus", "/tmp/outside"),
            "/tmp/outside"
        );
    }

    #[test]
    fn no_gpt_root_and_needs_recovery() {
        let disk = wrap_bytes(&vec![0u8; 4096]);
        let (class, facts) = probe_disk(&disk.0);
        assert_eq!(class, ImageClass::Unsupported("no GPT root".into()));
        assert!(!facts.gpt_detail.is_empty());

        let mut img = sample_ext4_image();
        let off = 1024 + 0x60;
        let mut incompat = u32::from_le_bytes(img[off..off + 4].try_into().unwrap());
        incompat |= 0x4;
        img[off..off + 4].copy_from_slice(&incompat.to_le_bytes());
        let disk = wrap(&img, ROOT_A);
        let (class, _) = probe_disk(&disk.0);
        assert!(
            matches!(class, ImageClass::Unknown(ref reason) if reason.contains("recovery")),
            "{class}"
        );
    }

    fn wrap_bytes(bytes: &[u8]) -> TempDisk {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vibe-probe-raw-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, bytes).unwrap();
        TempDisk(path)
    }

    #[test]
    fn verify_processed_accepts_only_a_clean_result() {
        let good = wrap(&gunzip("ext4-new.img.gz"), ROOT_A);
        let (class, before) = probe_disk(&good.0);
        assert_eq!(class, ImageClass::LegacyCompatible);
        assert_eq!(before.debian_version.as_deref(), Some("13.7\n"));
        assert_eq!(before.machine_id, MachineIdFact::EmptyFile);
        assert_eq!(before.root_line_count, Some(1));
        assert_eq!(before.root_password_empty, Some(true));
        assert_eq!(before.dbus, DbusState::LinkToEtc);
        assert_eq!(verify_processed(&good.0, &before), Ok(()));

        let nonempty = wrap(&gunzip("ext4-mid-nonempty.img.gz"), ROOT_A);
        let (class, facts) = probe_disk(&nonempty.0);
        assert_eq!(class, ImageClass::LegacyCompatible);
        assert_eq!(facts.machine_id, MachineIdFact::Valid);
        assert_eq!(
            verify_processed(&nonempty.0, &facts),
            Err(VerifyError::MachineIdNotEmpty)
        );

        let link = wrap(&gunzip("ext4-mid-symlink.img.gz"), ROOT_A);
        let (class, facts) = probe_disk(&link.0);
        assert_eq!(class, ImageClass::Unknown("machine-id is a symlink".into()));
        assert_eq!(
            verify_processed(&link.0, &facts),
            Err(VerifyError::MachineIdSymlink)
        );

        let locked = wrap(&gunzip("ext4-shadow-locked.img.gz"), ROOT_A);
        let (class, facts) = probe_disk(&locked.0);
        assert_eq!(class, ImageClass::NeedsPreparation);
        assert_eq!(facts.root_password_empty, Some(false));
        let rendered = format!("{facts:?} {class}");
        assert!(
            !rendered.contains("!unprovisioned"),
            "shadow plaintext leaked into diagnostics"
        );
        assert_eq!(
            verify_processed(&locked.0, &facts),
            Err(VerifyError::RootPasswordNotEmpty)
        );

        let two = wrap(&gunzip("ext4-shadow-two.img.gz"), ROOT_A);
        let (class, facts) = probe_disk(&two.0);
        assert_eq!(
            class,
            ImageClass::Unknown("root account line count is 2".into())
        );
        assert_eq!(
            verify_processed(&two.0, &facts),
            Err(VerifyError::RootLineCount)
        );

        let none = wrap(&gunzip("ext4-shadow-none.img.gz"), ROOT_A);
        let (_class, facts) = probe_disk(&none.0);
        assert_eq!(facts.root_line_count, Some(0));
        assert_eq!(
            verify_processed(&none.0, &facts),
            Err(VerifyError::RootLineCount)
        );

        let outside = wrap(&gunzip("ext4-dbus-outside.img.gz"), ROOT_A);
        let (class, facts) = probe_disk(&outside.0);
        assert_eq!(class, ImageClass::LegacyCompatible);
        assert_eq!(facts.dbus, DbusState::Other);
        assert_eq!(
            verify_processed(&outside.0, &facts),
            Err(VerifyError::DbusMachineId)
        );

        let moved = wrap(&gunzip("ext4-new.img.gz"), ROOT_B);
        assert_eq!(
            verify_processed(&moved.0, &before),
            Err(VerifyError::LayoutChanged)
        );
    }

    #[test]
    #[ignore = "set VIBE_TEST_DEBIAN_NEW_RAW, VIBE_TEST_DEBIAN_OLD_RAW, VIBE_TEST_ALPINE_RAW; VIBE_TEST_DEBIAN_LOCKED_ROOT_RAW is optional"]
    fn real_images_match_recorded_classes() {
        let Some(new) = std::env::var_os("VIBE_TEST_DEBIAN_NEW_RAW") else {
            eprintln!("skipping real image probe; set VIBE_TEST_DEBIAN_NEW_RAW");
            return;
        };
        let Some(old) = std::env::var_os("VIBE_TEST_DEBIAN_OLD_RAW") else {
            eprintln!("skipping real image probe; set VIBE_TEST_DEBIAN_OLD_RAW");
            return;
        };
        let Some(alpine) = std::env::var_os("VIBE_TEST_ALPINE_RAW") else {
            eprintln!("skipping real image probe; set VIBE_TEST_ALPINE_RAW");
            return;
        };

        let (class, facts) = probe_disk(Path::new(&new));
        assert_eq!(class, ImageClass::NeedsPreparation);
        assert_eq!(facts.machine_id, MachineIdFact::Uninitialized);
        assert_eq!(facts.root_line_count, Some(1));
        assert_eq!(facts.root_password_empty, Some(false));
        assert_eq!(facts.dbus, DbusState::Missing);
        assert!(
            facts
                .debian_version
                .as_deref()
                .unwrap_or("")
                .starts_with("13.7")
        );

        let (class, facts) = probe_disk(Path::new(&old));
        assert_eq!(class, ImageClass::LegacyCompatible);
        assert_eq!(facts.machine_id, MachineIdFact::EmptyFile);
        assert_eq!(facts.root_line_count, Some(1));
        assert_eq!(facts.root_password_empty, Some(true));
        assert!(
            facts
                .debian_version
                .as_deref()
                .unwrap_or("")
                .starts_with("13.3")
        );

        let (class, _) = probe_disk(Path::new(&alpine));
        assert_eq!(class, ImageClass::Unsupported("not Debian".into()));

        // A new Debian copy whose machine-id was cleared while root stayed
        // locked still needs preparation.
        match std::env::var_os("VIBE_TEST_DEBIAN_LOCKED_ROOT_RAW") {
            None => eprintln!(
                "VIBE_TEST_DEBIAN_LOCKED_ROOT_RAW is unset; skipping the cleared machine-id, locked root case"
            ),
            Some(path) => {
                let (class, facts) = probe_disk(Path::new(&path));
                assert_eq!(class, ImageClass::NeedsPreparation);
                assert_eq!(facts.machine_id, MachineIdFact::EmptyFile);
                assert_eq!(facts.root_password_empty, Some(false));
            }
        }
    }

    #[test]
    fn shadow_parser_ignores_similar_names() {
        let (count, empty) = parse_shadow(b"rootuser:x\nroot::1::::::\n");
        assert_eq!((count, empty), (1, Some(true)));
        let (count, empty) = parse_shadow(b"root:secret:1::::::\n");
        assert_eq!((count, empty), (1, Some(false)));
    }
}
