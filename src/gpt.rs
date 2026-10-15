//! Read-only GPT parsing and filesystem label scan.
//!
//! Used to find the single Linux root partition and the ESP on a disk image,
//! and to reject label or UUID clashes before a maintenance VM starts.
//! The parser checks the primary header at LBA 1 only; it does not fall back
//! to the backup header.
//!
//! TODO(T18): Alpine image unpack checks partition 2's ext4 UUID.
//! TODO(T21): maintenance safety check calls `scan_labels`.
//! TODO(T28): `probe_disk` calls `read_gpt` and `select_layout`.

#![allow(dead_code)]

use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};

/// EFI System Partition type GUID.
pub(crate) const ESP_TYPE_GUID: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// Generic Linux filesystem type GUID.
pub(crate) const LINUX_FILESYSTEM_GUID: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// Discoverable Partitions Spec: Linux root (ARM64).
pub(crate) const LINUX_ROOT_ARM64_GUID: &str = "b921b045-1df0-41c3-af44-4c6f280d3fae";

const SECTOR_BYTES: u64 = 512;
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const HEADER_BYTES_MIN: u32 = 92;
/// More than this many partition entries is rejected before the array is read.
const MAX_PARTITION_ENTRIES: u32 = 1024;
/// Partition entry size must be at least 128 and is capped here.
const MAX_ENTRY_SIZE: u32 = 4096;
const MAX_PARTITION_ARRAY_BYTES: u64 = 1024 * 1024;

/// One used GPT partition entry. `index` is the 1-based slot in the entry array
/// (empty slots still consume a number, so a Debian ESP in slot 15 is index 15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Partition {
    pub index: u32,
    pub type_guid: String,
    pub unique_guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: String,
}

/// Primary GPT, with unused (all-zero type) entries omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gpt {
    pub partitions: Vec<Partition>,
}

/// The single Linux root and the single ESP selected for a disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layout {
    pub root: Partition,
    pub esp: Partition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsKind {
    Ext4,
    Fat,
    Iso9660,
}

/// A filesystem label found at a partition or at byte offset 0 of the disk.
/// `part` is `None` for the whole-disk probe. `uuid` is empty when the
/// filesystem has no ext4-style UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FsLabel {
    pub part: Option<u32>,
    pub kind: FsKind,
    pub label: String,
    pub uuid: String,
}

#[derive(Debug)]
pub(crate) enum GptError {
    Io(io::Error),
    BadSignature,
    BadHeader,
    BadHeaderCrc,
    BadEntriesCrc,
    EntrySize,
    EntryCount,
    NoRoot,
    MultipleRoots,
    NoEsp,
    MultipleEsp,
    OutOfBounds { index: u32 },
    Overflow,
}

impl std::fmt::Display for GptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GptError::Io(err) => write!(f, "gpt read error: {err}"),
            GptError::BadSignature => write!(f, "gpt header signature is not EFI PART"),
            GptError::BadHeader => write!(f, "gpt header fields are not usable"),
            GptError::BadHeaderCrc => write!(f, "gpt header CRC32 does not match"),
            GptError::BadEntriesCrc => write!(f, "gpt partition array CRC32 does not match"),
            GptError::EntrySize => write!(f, "gpt partition entry size is not supported"),
            GptError::EntryCount => write!(f, "gpt partition entry count is over the limit"),
            GptError::NoRoot => write!(f, "gpt has no Linux root partition"),
            GptError::MultipleRoots => write!(f, "gpt has more than one Linux root partition"),
            GptError::NoEsp => write!(f, "gpt has no EFI system partition"),
            GptError::MultipleEsp => write!(f, "gpt has more than one EFI system partition"),
            GptError::OutOfBounds { index } => {
                write!(f, "gpt partition {index} is outside the disk")
            }
            GptError::Overflow => write!(f, "gpt size calculation overflowed"),
        }
    }
}

impl std::error::Error for GptError {}

impl From<io::Error> for GptError {
    fn from(err: io::Error) -> Self {
        GptError::Io(err)
    }
}

/// Read and check the primary GPT header and partition array.
pub(crate) fn read_gpt(file: &mut File) -> Result<Gpt, GptError> {
    let mut header = [0u8; SECTOR_BYTES as usize];
    file.seek(SeekFrom::Start(SECTOR_BYTES))?;
    file.read_exact(&mut header)?;
    if &header[0..8] != GPT_SIGNATURE {
        return Err(GptError::BadSignature);
    }

    let header_size = read_u32(&header, 0x0C);
    if header_size < HEADER_BYTES_MIN || header_size as u64 > SECTOR_BYTES {
        return Err(GptError::BadHeader);
    }
    let stored_crc = read_u32(&header, 0x10);
    let mut crc_buf = header[..header_size as usize].to_vec();
    crc_buf[0x10..0x14].fill(0);
    if crc32(&crc_buf) != stored_crc {
        return Err(GptError::BadHeaderCrc);
    }

    let my_lba = read_u64(&header, 0x18);
    if my_lba != 1 {
        return Err(GptError::BadHeader);
    }

    let entry_lba = read_u64(&header, 0x48);
    let entry_count = read_u32(&header, 0x50);
    let entry_size = read_u32(&header, 0x54);
    let entries_crc = read_u32(&header, 0x58);
    if entry_size < 128 || !entry_size.is_multiple_of(8) || entry_size > MAX_ENTRY_SIZE {
        return Err(GptError::EntrySize);
    }
    if entry_count == 0 || entry_count > MAX_PARTITION_ENTRIES {
        return Err(GptError::EntryCount);
    }
    let array_bytes = (entry_count as u64)
        .checked_mul(entry_size as u64)
        .ok_or(GptError::Overflow)?;
    if array_bytes > MAX_PARTITION_ARRAY_BYTES {
        return Err(GptError::EntryCount);
    }
    let array_off = entry_lba
        .checked_mul(SECTOR_BYTES)
        .ok_or(GptError::Overflow)?;

    let mut array = vec![0u8; array_bytes as usize];
    file.seek(SeekFrom::Start(array_off))?;
    file.read_exact(&mut array)?;
    if crc32(&array) != entries_crc {
        return Err(GptError::BadEntriesCrc);
    }

    let mut partitions = Vec::new();
    for index in 0..entry_count {
        let start = index as usize * entry_size as usize;
        let entry = &array[start..start + entry_size as usize];
        if entry[..16].iter().all(|b| *b == 0) {
            continue;
        }
        let first_lba = read_u64(entry, 0x20);
        let last_lba = read_u64(entry, 0x28);
        partitions.push(Partition {
            index: index + 1,
            type_guid: format_guid(&entry[0..16]),
            unique_guid: format_guid(&entry[16..32]),
            first_lba,
            last_lba,
            name: decode_utf16_name(&entry[0x38..0x38 + 72]),
        });
    }
    Ok(Gpt { partitions })
}

/// Select exactly one Linux root and exactly one ESP.
///
/// A root is an ARM64 root partition (`b921b045-…`) or, when no ARM64 root
/// exists, a Linux filesystem partition (`0fc63daf-…`). If both types are
/// present, the ARM64 root is used and it must be the only one of that type.
/// Every used partition must lie inside `disk_bytes`.
pub(crate) fn select_layout(gpt: &Gpt, disk_bytes: u64) -> Result<Layout, GptError> {
    for part in &gpt.partitions {
        if part.last_lba < part.first_lba || part.first_lba == 0 {
            return Err(GptError::OutOfBounds { index: part.index });
        }
        let start = part
            .first_lba
            .checked_mul(SECTOR_BYTES)
            .ok_or(GptError::Overflow)?;
        let sectors = part
            .last_lba
            .checked_sub(part.first_lba)
            .and_then(|n| n.checked_add(1))
            .ok_or(GptError::Overflow)?;
        let len = sectors
            .checked_mul(SECTOR_BYTES)
            .ok_or(GptError::Overflow)?;
        let end = start.checked_add(len).ok_or(GptError::Overflow)?;
        if end > disk_bytes {
            return Err(GptError::OutOfBounds { index: part.index });
        }
    }

    let arm64: Vec<&Partition> = gpt
        .partitions
        .iter()
        .filter(|p| p.type_guid == LINUX_ROOT_ARM64_GUID)
        .collect();
    let linux_fs: Vec<&Partition> = gpt
        .partitions
        .iter()
        .filter(|p| p.type_guid == LINUX_FILESYSTEM_GUID)
        .collect();
    let root = if !arm64.is_empty() {
        if arm64.len() != 1 {
            return Err(GptError::MultipleRoots);
        }
        arm64[0]
    } else if linux_fs.len() == 1 {
        linux_fs[0]
    } else if linux_fs.is_empty() {
        return Err(GptError::NoRoot);
    } else {
        return Err(GptError::MultipleRoots);
    };

    let esps: Vec<&Partition> = gpt
        .partitions
        .iter()
        .filter(|p| p.type_guid == ESP_TYPE_GUID)
        .collect();
    let esp = match esps.as_slice() {
        [one] => *one,
        [] => return Err(GptError::NoEsp),
        _ => return Err(GptError::MultipleEsp),
    };
    Ok(Layout {
        root: root.clone(),
        esp: esp.clone(),
    })
}

/// Probe ext4, FAT, and ISO9660 labels at every used partition and at byte 0.
pub(crate) fn scan_labels(file: &mut File, gpt: &Gpt) -> Result<Vec<FsLabel>, GptError> {
    let disk_bytes = file.seek(SeekFrom::End(0))?;
    let mut labels = Vec::new();
    if let Some(label) = read_fs_label(file, 0, disk_bytes, None)? {
        labels.push(label);
    }
    for part in &gpt.partitions {
        if part.last_lba < part.first_lba {
            continue;
        }
        let offset = part
            .first_lba
            .checked_mul(SECTOR_BYTES)
            .ok_or(GptError::Overflow)?;
        let sectors = part
            .last_lba
            .checked_sub(part.first_lba)
            .and_then(|n| n.checked_add(1))
            .ok_or(GptError::Overflow)?;
        let len = sectors
            .checked_mul(SECTOR_BYTES)
            .ok_or(GptError::Overflow)?;
        if let Some(label) = read_fs_label(file, offset, len, Some(part.index))? {
            labels.push(label);
        }
    }
    Ok(labels)
}

fn read_fs_label(
    file: &mut File,
    offset: u64,
    len: u64,
    part: Option<u32>,
) -> Result<Option<FsLabel>, GptError> {
    if let Some(label) = read_ext4_label(file, offset, len)? {
        return Ok(Some(FsLabel {
            part,
            kind: FsKind::Ext4,
            label: label.0,
            uuid: label.1,
        }));
    }
    if let Some(label) = read_fat_label(file, offset, len)? {
        return Ok(Some(FsLabel {
            part,
            kind: FsKind::Fat,
            label,
            uuid: String::new(),
        }));
    }
    if let Some(label) = read_iso_label(file, offset, len)? {
        return Ok(Some(FsLabel {
            part,
            kind: FsKind::Iso9660,
            label,
            uuid: String::new(),
        }));
    }
    Ok(None)
}

fn read_exact_at(file: &mut File, offset: u64, buf: &mut [u8]) -> Result<(), GptError> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buf)?;
    Ok(())
}

/// ext4 superblock magic at +1024, label at +0x78, UUID at +0x68.
fn read_ext4_label(
    file: &mut File,
    offset: u64,
    len: u64,
) -> Result<Option<(String, String)>, GptError> {
    const NEED: u64 = 1024 + 0x88;
    if len < NEED {
        return Ok(None);
    }
    let mut sb = [0u8; 0x88];
    if read_exact_at(file, offset + 1024, &mut sb).is_err() {
        return Ok(None);
    }
    if read_u16(&sb, 0x38) != 0xEF53 {
        return Ok(None);
    }
    let log_block = read_u32(&sb, 0x18);
    if log_block > 6 {
        return Ok(None);
    }
    let label = c_string(&sb[0x78..0x88]);
    let uuid = format_ext4_uuid(&sb[0x68..0x78]);
    Ok(Some((label, uuid)))
}

/// FAT boot sector: label at 0x47 for FAT32, otherwise at 0x2B.
fn read_fat_label(file: &mut File, offset: u64, len: u64) -> Result<Option<String>, GptError> {
    if len < 512 {
        return Ok(None);
    }
    let mut sector = [0u8; 512];
    if read_exact_at(file, offset, &mut sector).is_err() {
        return Ok(None);
    }
    if sector[0x1FE] != 0x55 || sector[0x1FF] != 0xAA {
        return Ok(None);
    }
    if sector[0] != 0xEB && sector[0] != 0xE9 {
        return Ok(None);
    }
    let raw = if sector[0x52..0x5A].starts_with(b"FAT32") {
        &sector[0x47..0x47 + 11]
    } else if sector[0x36..0x3E].starts_with(b"FAT") {
        &sector[0x2B..0x2B + 11]
    } else {
        return Ok(None);
    };
    Ok(Some(trim_padded(raw)))
}

/// ISO9660 Primary Volume Descriptor volume id at offset 0x8028.
fn read_iso_label(file: &mut File, offset: u64, len: u64) -> Result<Option<String>, GptError> {
    const LABEL_OFF: u64 = 0x8028;
    const LABEL_LEN: usize = 32;
    if len < LABEL_OFF + LABEL_LEN as u64 {
        return Ok(None);
    }
    let mut pvd_head = [0u8; 6];
    if read_exact_at(file, offset + 0x8000, &mut pvd_head).is_err() {
        return Ok(None);
    }
    if pvd_head[0] != 1 || &pvd_head[1..6] != b"CD001" {
        return Ok(None);
    }
    let mut label = [0u8; LABEL_LEN];
    if read_exact_at(file, offset + LABEL_OFF, &mut label).is_err() {
        return Ok(None);
    }
    Ok(Some(trim_padded(&label)))
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn trim_padded(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .rposition(|b| *b != 0 && *b != b' ')
        .map(|i| i + 1)
        .unwrap_or(0);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// GPT GUID: mixed endian, lowercase canonical text.
pub(crate) fn format_guid(bytes: &[u8]) -> String {
    let d1 = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let d2 = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
    let d3 = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
    format!(
        "{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// ext4 `s_uuid` is stored in the order `blkid` prints (not GPT mixed endian).
pub(crate) fn format_ext4_uuid(bytes: &[u8]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

fn decode_utf16_name(bytes: &[u8]) -> String {
    let mut units = Vec::new();
    for chunk in bytes.chunks(2) {
        if chunk.len() < 2 {
            break;
        }
        let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}

fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn read_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

/// ISO-HDLC CRC-32 (the polynomial GPT uses).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            if crc & 1 == 1 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestPart {
    pub index: u32,
    pub type_guid: &'static str,
    pub unique_guid: &'static str,
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: &'static str,
}

#[cfg(test)]
fn guid_to_mixed_bytes(canonical: &str) -> [u8; 16] {
    let hex: String = canonical.chars().filter(|c| *c != '-').collect();
    assert_eq!(hex.len(), 32, "guid {canonical}");
    let mut raw = [0u8; 16];
    for i in 0..16 {
        raw[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&u32::from_be_bytes(raw[0..4].try_into().unwrap()).to_le_bytes());
    out[4..6].copy_from_slice(&u16::from_be_bytes(raw[4..6].try_into().unwrap()).to_le_bytes());
    out[6..8].copy_from_slice(&u16::from_be_bytes(raw[6..8].try_into().unwrap()).to_le_bytes());
    out[8..16].copy_from_slice(&raw[8..16]);
    out
}

/// Build a primary GPT image. The buffer is only as long as the entry array;
/// callers pass `disk_bytes` separately to [`select_layout`].
#[cfg(test)]
pub(crate) fn build_gpt(parts: &[TestPart], disk_bytes: u64, entry_count: u32) -> Vec<u8> {
    let entry_size: u32 = 128;
    let entry_lba = 2u64;
    let array_bytes = entry_count as usize * entry_size as usize;
    let mut array = vec![0u8; array_bytes];
    for part in parts {
        assert!(part.index >= 1 && part.index <= entry_count);
        let start = (part.index as usize - 1) * entry_size as usize;
        let entry = &mut array[start..start + entry_size as usize];
        entry[0..16].copy_from_slice(&guid_to_mixed_bytes(part.type_guid));
        entry[16..32].copy_from_slice(&guid_to_mixed_bytes(part.unique_guid));
        entry[0x20..0x28].copy_from_slice(&part.first_lba.to_le_bytes());
        entry[0x28..0x30].copy_from_slice(&part.last_lba.to_le_bytes());
        let mut units: Vec<u16> = part.name.encode_utf16().collect();
        units.truncate(36);
        for (i, unit) in units.iter().enumerate() {
            entry[0x38 + i * 2..0x38 + i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    let entries_crc = crc32(&array);

    let mut header = [0u8; 512];
    header[0..8].copy_from_slice(GPT_SIGNATURE);
    header[0x08..0x0C].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[0x0C..0x10].copy_from_slice(&92u32.to_le_bytes());
    header[0x18..0x20].copy_from_slice(&1u64.to_le_bytes());
    let backup_lba = disk_bytes / SECTOR_BYTES - 1;
    header[0x20..0x28].copy_from_slice(&backup_lba.to_le_bytes());
    header[0x28..0x30].copy_from_slice(&34u64.to_le_bytes());
    header[0x30..0x38].copy_from_slice(&(backup_lba.saturating_sub(1)).to_le_bytes());
    header[0x48..0x50].copy_from_slice(&entry_lba.to_le_bytes());
    header[0x50..0x54].copy_from_slice(&entry_count.to_le_bytes());
    header[0x54..0x58].copy_from_slice(&entry_size.to_le_bytes());
    header[0x58..0x5C].copy_from_slice(&entries_crc.to_le_bytes());
    let header_crc = crc32(&header[..92]);
    header[0x10..0x14].copy_from_slice(&header_crc.to_le_bytes());

    let mut image = vec![0u8; (entry_lba as usize) * 512 + array_bytes];
    image[512..1024].copy_from_slice(&header);
    image[1024..1024 + array_bytes].copy_from_slice(&array);
    image
}

#[cfg(test)]
fn write_temp(bytes: &[u8]) -> File {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vibe-gpt-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, bytes).unwrap();
    let file = File::open(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    file
}

#[cfg(test)]
fn sample_parts() -> Vec<TestPart> {
    vec![
        TestPart {
            index: 1,
            type_guid: LINUX_ROOT_ARM64_GUID,
            unique_guid: "11111111-2222-3333-4444-555555555555",
            first_lba: 4096,
            last_lba: 8191,
            name: "root",
        },
        TestPart {
            index: 15,
            type_guid: ESP_TYPE_GUID,
            unique_guid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            first_lba: 2048,
            last_lba: 4095,
            name: "EFI",
        },
    ]
}

#[cfg(test)]
const DISK_BYTES: u64 = 8 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_iso_hdlc_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn normal_layout_round_trip() {
        let image = build_gpt(&sample_parts(), DISK_BYTES, 128);
        let mut file = write_temp(&image);
        let gpt = read_gpt(&mut file).unwrap();
        assert_eq!(gpt.partitions.len(), 2);
        let root = gpt.partitions.iter().find(|p| p.index == 1).unwrap();
        assert_eq!(root.type_guid, LINUX_ROOT_ARM64_GUID);
        assert_eq!(root.name, "root");
        assert_eq!(root.first_lba, 4096);
        let layout = select_layout(&gpt, DISK_BYTES).unwrap();
        assert_eq!(layout.root.index, 1);
        assert_eq!(layout.esp.index, 15);
        assert_eq!(layout.esp.type_guid, ESP_TYPE_GUID);
    }

    #[test]
    fn header_and_entry_crc_errors() {
        let mut image = build_gpt(&sample_parts(), DISK_BYTES, 128);
        image[512 + 0x20] ^= 0xFF;
        let mut file = write_temp(&image);
        assert!(matches!(read_gpt(&mut file), Err(GptError::BadHeaderCrc)));

        let mut image = build_gpt(&sample_parts(), DISK_BYTES, 128);
        image[1024] ^= 0xFF;
        let mut file = write_temp(&image);
        assert!(matches!(read_gpt(&mut file), Err(GptError::BadEntriesCrc)));
    }

    #[test]
    fn entry_count_over_the_limit_is_rejected() {
        let image = build_gpt(&[], DISK_BYTES, MAX_PARTITION_ENTRIES + 1);
        let mut file = write_temp(&image);
        assert!(matches!(read_gpt(&mut file), Err(GptError::EntryCount)));
    }

    #[test]
    fn missing_and_duplicate_roots_and_esp() {
        let disk = DISK_BYTES;
        let esp = sample_parts().pop().unwrap();
        let only_esp = build_gpt(std::slice::from_ref(&esp), disk, 128);
        let mut file = write_temp(&only_esp);
        let gpt = read_gpt(&mut file).unwrap();
        assert!(matches!(select_layout(&gpt, disk), Err(GptError::NoRoot)));

        let two_roots = vec![
            TestPart {
                index: 1,
                type_guid: LINUX_ROOT_ARM64_GUID,
                unique_guid: "11111111-2222-3333-4444-555555555555",
                first_lba: 4096,
                last_lba: 5000,
                name: "",
            },
            TestPart {
                index: 2,
                type_guid: LINUX_ROOT_ARM64_GUID,
                unique_guid: "22222222-2222-3333-4444-555555555555",
                first_lba: 5001,
                last_lba: 6000,
                name: "",
            },
            esp.clone(),
        ];
        let mut file = write_temp(&build_gpt(&two_roots, disk, 128));
        let gpt = read_gpt(&mut file).unwrap();
        assert!(matches!(
            select_layout(&gpt, disk),
            Err(GptError::MultipleRoots)
        ));

        let linux_only = vec![
            TestPart {
                index: 2,
                type_guid: LINUX_FILESYSTEM_GUID,
                unique_guid: "33333333-2222-3333-4444-555555555555",
                first_lba: 4096,
                last_lba: 8191,
                name: "alpine",
            },
            esp.clone(),
        ];
        let mut file = write_temp(&build_gpt(&linux_only, disk, 128));
        let gpt = read_gpt(&mut file).unwrap();
        let layout = select_layout(&gpt, disk).unwrap();
        assert_eq!(layout.root.index, 2);
        assert_eq!(layout.root.type_guid, LINUX_FILESYSTEM_GUID);

        let both = vec![
            TestPart {
                index: 1,
                type_guid: LINUX_FILESYSTEM_GUID,
                unique_guid: "33333333-2222-3333-4444-555555555555",
                first_lba: 4096,
                last_lba: 5000,
                name: "",
            },
            TestPart {
                index: 2,
                type_guid: LINUX_ROOT_ARM64_GUID,
                unique_guid: "44444444-2222-3333-4444-555555555555",
                first_lba: 5001,
                last_lba: 6000,
                name: "",
            },
            esp,
        ];
        let mut file = write_temp(&build_gpt(&both, disk, 128));
        let gpt = read_gpt(&mut file).unwrap();
        let layout = select_layout(&gpt, disk).unwrap();
        assert_eq!(layout.root.index, 2);
    }

    #[test]
    fn partition_past_the_end_of_the_disk() {
        let mut parts = sample_parts();
        parts[0].last_lba = 100_000;
        let mut file = write_temp(&build_gpt(&parts, DISK_BYTES, 128));
        let gpt = read_gpt(&mut file).unwrap();
        assert!(matches!(
            select_layout(&gpt, DISK_BYTES),
            Err(GptError::OutOfBounds { index: 1 })
        ));
    }

    #[test]
    fn no_esp_is_rejected() {
        let root = sample_parts().into_iter().next().unwrap();
        let mut file = write_temp(&build_gpt(std::slice::from_ref(&root), DISK_BYTES, 128));
        let gpt = read_gpt(&mut file).unwrap();
        assert!(matches!(
            select_layout(&gpt, DISK_BYTES),
            Err(GptError::NoEsp)
        ));
    }

    #[test]
    fn labels_ext4_fat_and_iso() {
        // ext4 at byte 0, FAT in the LBA 8 sector, ISO9660 in a partition that
        // starts at LBA 16 (primary volume descriptor at partition+0x8000).
        let iso_lba = 16u64;
        // 65 sectors cover the primary volume descriptor and its 32-byte label.
        let mut image = vec![0u8; ((iso_lba + 65) * 512) as usize];
        let pvd = (iso_lba * 512 + 0x8000) as usize;
        let uuid: [u8; 16] = [
            0xd5, 0x1a, 0x62, 0x13, 0xc7, 0x65, 0x4b, 0x85, 0x91, 0x05, 0x1b, 0x43, 0x8c, 0x52,
            0x39, 0x93,
        ];
        image[1024 + 0x18..1024 + 0x1C].copy_from_slice(&2u32.to_le_bytes());
        image[1024 + 0x38..1024 + 0x3A].copy_from_slice(&0xEF53u16.to_le_bytes());
        image[1024 + 0x68..1024 + 0x78].copy_from_slice(&uuid);
        image[1024 + 0x78] = b'/';

        let fat = 4096;
        image[fat] = 0xEB;
        image[fat + 0x47..fat + 0x47 + 11].copy_from_slice(b"CIDATA     ");
        image[fat + 0x52..fat + 0x5A].copy_from_slice(b"FAT32   ");
        image[fat + 0x1FE] = 0x55;
        image[fat + 0x1FF] = 0xAA;

        image[pvd] = 1;
        image[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        image[pvd + 0x28..pvd + 0x28 + 6].copy_from_slice(b"cidata");

        let gpt = Gpt {
            partitions: vec![
                Partition {
                    index: 1,
                    type_guid: LINUX_FILESYSTEM_GUID.into(),
                    unique_guid: "11111111-2222-3333-4444-555555555555".into(),
                    first_lba: 8,
                    last_lba: 8,
                    name: String::new(),
                },
                Partition {
                    index: 2,
                    type_guid: LINUX_FILESYSTEM_GUID.into(),
                    unique_guid: "22222222-2222-3333-4444-555555555555".into(),
                    first_lba: iso_lba,
                    last_lba: iso_lba + 64,
                    name: String::new(),
                },
            ],
        };
        let mut file = write_temp(&image);
        let labels = scan_labels(&mut file, &gpt).unwrap();
        let ext4 = labels
            .iter()
            .find(|l| l.kind == FsKind::Ext4 && l.part.is_none())
            .unwrap();
        assert_eq!(ext4.label, "/");
        assert_eq!(ext4.uuid, "d51a6213-c765-4b85-9105-1b438c523993");
        let fat = labels
            .iter()
            .find(|l| l.kind == FsKind::Fat && l.part == Some(1))
            .unwrap();
        assert_eq!(fat.label, "CIDATA");
        let iso = labels
            .iter()
            .find(|l| l.kind == FsKind::Iso9660 && l.part == Some(2))
            .unwrap();
        assert_eq!(iso.label, "cidata");
    }

    #[test]
    #[ignore = "set VIBE_TEST_DEBIAN_NEW_RAW, VIBE_TEST_DEBIAN_OLD_RAW, and VIBE_TEST_ALPINE_RAW"]
    fn real_disk_layouts() {
        let new = std::env::var("VIBE_TEST_DEBIAN_NEW_RAW").ok();
        let old = std::env::var("VIBE_TEST_DEBIAN_OLD_RAW").ok();
        let alpine = std::env::var("VIBE_TEST_ALPINE_RAW").ok();
        if new.is_none() || old.is_none() || alpine.is_none() {
            eprintln!(
                "skipping real GPT fixtures; set VIBE_TEST_DEBIAN_NEW_RAW, VIBE_TEST_DEBIAN_OLD_RAW, VIBE_TEST_ALPINE_RAW"
            );
            return;
        }
        for path in [new.unwrap(), old.unwrap()] {
            let mut file = File::open(&path).unwrap();
            let len = file.metadata().unwrap().len();
            let gpt = read_gpt(&mut file).unwrap();
            let layout = select_layout(&gpt, len).unwrap();
            assert_eq!(layout.root.index, 1, "{path}");
            assert_eq!(layout.esp.index, 15, "{path}");
            let labels = scan_labels(&mut file, &gpt).unwrap();
            let root_label = labels
                .iter()
                .find(|l| l.kind == FsKind::Ext4 && l.part == Some(layout.root.index));
            assert!(root_label.is_some(), "{path}");
            assert_eq!(root_label.unwrap().label, "", "{path}");
        }
        let alpine = alpine.unwrap();
        let mut file = File::open(&alpine).unwrap();
        let len = file.metadata().unwrap().len();
        let gpt = read_gpt(&mut file).unwrap();
        let layout = select_layout(&gpt, len).unwrap();
        assert_eq!(layout.root.index, 2);
        let labels = scan_labels(&mut file, &gpt).unwrap();
        let root = labels
            .iter()
            .find(|l| l.kind == FsKind::Ext4 && l.part == Some(2))
            .unwrap();
        assert_eq!(root.label, "/");
        assert_eq!(root.uuid, "d51a6213-c765-4b85-9105-1b438c523993");
    }
}
