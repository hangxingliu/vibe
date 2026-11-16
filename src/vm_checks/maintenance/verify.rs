use std::path::Path;

use crate::ext4ro::FileMeta;

use super::{
    UNCHANGED,
    disk_read::{layout_of, lstat_required, read_any, read_regular, shadow_group_gid},
    util::sha256_range,
};

pub(super) fn check_processed(
    disk: &Path,
    before: &Snapshot,
) -> Result<(), Box<dyn std::error::Error>> {
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

pub(super) struct Snapshot {
    files: Vec<Option<Vec<u8>>>,
    shadow: Vec<u8>,
    shadow_meta: FileMeta,
    machine_meta: FileMeta,
    shadow_gid: Option<u32>,
    esp_sha: String,
}

pub(super) fn snapshot(disk: &Path) -> Result<Snapshot, Box<dyn std::error::Error>> {
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

/// Checks that the only change between two `/etc/shadow` files is a cleared
/// root password.
///
/// Both files must have the same number of lines and the same trailing
/// newline. Every line that does not start with `root:` must be byte-equal.
/// Exactly one line may start with `root:`. On that line the `:`-separated
/// field count must match and be at least 2, field 1 (the password hash)
/// must be empty after the change, and all other fields (name, last change,
/// min, max, warn, inactive, expire, reserved) must be byte-equal.
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

pub(super) fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<_> = bytes.split(|b| *b == b'\n').collect();
    if bytes.ends_with(b"\n") {
        lines.pop();
    }
    lines
}

pub(super) fn split_fields(line: &[u8]) -> Vec<Vec<u8>> {
    line.split(|b| *b == b':').map(|f| f.to_vec()).collect()
}
