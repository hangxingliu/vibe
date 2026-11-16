use std::{fs, path::Path};

use crate::{
    ext4ro::{Ext4Ro, FileMeta, FileRead},
    gpt::{self, Layout},
};

use super::verify::{split_fields, split_lines};

pub(super) fn layout_of(disk: &Path) -> Result<Layout, Box<dyn std::error::Error>> {
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

pub(super) fn read_regular(disk: &Path, path: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    match read_any(disk, path)? {
        Some(bytes) => Ok(bytes),
        None => Err(format!("{path} is missing").into()),
    }
}

pub(super) fn read_any(
    disk: &Path,
    path: &str,
) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    let (file, offset, len) = open_root(disk)?;
    let fs = Ext4Ro::open(&file, offset, len)?;
    match fs.read_file(path, 1024 * 1024)? {
        FileRead::Regular(bytes) => Ok(Some(bytes)),
        FileRead::Symlink(target) => Ok(Some(target.into_bytes())),
        FileRead::Missing => Ok(None),
        FileRead::Other => Ok(None),
    }
}

pub(super) fn read_kind(
    disk: &Path,
    path: &str,
) -> Result<Option<FileRead>, Box<dyn std::error::Error>> {
    let (file, offset, len) = open_root(disk)?;
    let fs = Ext4Ro::open(&file, offset, len)?;
    match fs.read_file(path, 64)? {
        FileRead::Missing => Ok(None),
        other => Ok(Some(other)),
    }
}

pub(super) fn lstat_required(
    disk: &Path,
    path: &str,
) -> Result<FileMeta, Box<dyn std::error::Error>> {
    let (file, offset, len) = open_root(disk)?;
    let fs = Ext4Ro::open(&file, offset, len)?;
    fs.lstat(path)?
        .ok_or_else(|| format!("{path} is missing").into())
}

pub(super) fn shadow_group_gid(disk: &Path) -> Result<Option<u32>, Box<dyn std::error::Error>> {
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
