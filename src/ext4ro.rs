//! Bounded read-only ext4.
//!
//! `probe_disk` uses this to read a handful of fixed paths from an untrusted
//! root partition. The reader never follows symbolic links: a symlink comes
//! back as its target string, including fast symlinks stored in `i_block`.
//! `needs_recovery` is rejected so a dirty journal is left for Alpine's
//! `e2fsck` instead of being ignored.
//!
//! `image_probe` and the maintenance VM checks read through this module.
//! Provision is a production caller, via `image_probe`. The run path is not.
//!
//! Read path: `open` reads the superblock at byte 1024 of the partition and
//! derives the block size, inode size and group geometry from it. The group
//! descriptor table starts in the block after `s_first_data_block`; from each
//! descriptor only the inode table location is kept. An inode number then
//! maps to a group and a slot in that group's inode table. File contents are
//! located through the extent tree whose root lives in the inode's `i_block`.
//! Directory contents are read the same way and scanned as linear entry
//! blocks (hashed directory indexes are read as plain blocks). A path is
//! resolved from the root inode (2) one component at a time: read the
//! directory inode, look the name up in its blocks, and continue with the
//! child inode until the final component.

use std::{
    fs::File,
    io::{self, ErrorKind},
    os::unix::fs::FileExt,
};

mod dir;
mod extent;
mod inode;

use inode::Inode;

const EXT4_MAGIC: u16 = 0xEF53;
const EXTENT_MAGIC: u16 = 0xF30A;
const EXTENTS_FL: u32 = 0x80000;
const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_RECOVER: u32 = 0x4;
const INCOMPAT_EXTENTS: u32 = 0x40;
const INCOMPAT_64BIT: u32 = 0x80;
const INCOMPAT_FLEX_BG: u32 = 0x200;
const INCOMPAT_DIRDATA: u32 = 0x1000;
const INCOMPAT_CSUM_SEED: u32 = 0x2000;
const INCOMPAT_LARGEDIR: u32 = 0x4000;
/// `metadata_csum_seed` is the same incompat bit as `csum_seed`.
const INCOMPAT_ALLOWED: u32 = INCOMPAT_FILETYPE
    | INCOMPAT_EXTENTS
    | INCOMPAT_64BIT
    | INCOMPAT_FLEX_BG
    | INCOMPAT_DIRDATA
    | INCOMPAT_CSUM_SEED
    | INCOMPAT_LARGEDIR;
const S_IFDIR: u16 = 0x4000;
const S_IFREG: u16 = 0x8000;
const S_IFLNK: u16 = 0xA000;
const S_IFMT: u16 = 0xF000;
/// Caps the inode table list built in `open`, so a superblock with a huge
/// group count cannot force a huge allocation or millions of reads.
const MAX_GROUPS: u64 = 100_000;
/// Caps extent tree depth, so a crafted tree cannot recurse deeply.
const MAX_EXTENT_DEPTH: u16 = 5;
/// Caps the extent tree blocks visited per inode, so index entries that point
/// back at shared blocks cannot fan out into an effectively endless walk.
const MAX_EXTENT_NODES: usize = 64;
/// Caps the extents collected per inode, bounding memory and the overlap sort.
const MAX_EXTENTS: usize = 4096;
/// Caps a symlink target, so a forged size cannot request a huge buffer.
const MAX_SYMLINK_LEN: u64 = 4096;
/// Stop a directory walk after this many bytes so a hostile tree cannot loop.
/// It also bounds the work for a directory with a forged huge size.
const DIR_SCAN_LIMIT: u64 = 4 * 1024 * 1024;
/// Caps path components, bounding the number of directory lookups per path.
const MAX_PATH_COMPONENTS: usize = 64;

pub(crate) trait ReadAt {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl ReadAt for File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        FileExt::read_at(self, buf, offset)
    }
}

impl ReadAt for &File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        FileExt::read_at(*self, buf, offset)
    }
}

#[derive(Debug)]
pub(crate) enum Ext4Error {
    Io(io::Error),
    Unsupported(&'static str),
    Corrupt(&'static str),
    TooLarge,
}

impl Ext4Error {
    pub(crate) fn is_needs_recovery(&self) -> bool {
        matches!(self, Ext4Error::Unsupported("needs_recovery"))
    }
}

impl std::fmt::Display for Ext4Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ext4Error::Io(err) => write!(f, "read error: {err}"),
            Ext4Error::Unsupported(reason) | Ext4Error::Corrupt(reason) => write!(f, "{reason}"),
            Ext4Error::TooLarge => write!(f, "file is larger than the read limit"),
        }
    }
}

impl std::error::Error for Ext4Error {}

/// Result of reading one path. Symbolic links are not followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileRead {
    Regular(Vec<u8>),
    Missing,
    Symlink(String),
    Other,
}

/// `lstat` of one path. The final symlink is not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileMeta {
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
}

enum Walked {
    Root,
    Missing,
    Found(u32),
}

pub(crate) struct Ext4Ro<R> {
    reader: R,
    offset: u64,
    len: u64,
    fs_bytes: u64,
    block_size: u64,
    inode_size: u64,
    inodes_per_group: u32,
    inodes_count: u32,
    blocks_count: u64,
    inode_tables: Vec<u64>,
    filetype: bool,
}

impl<R: ReadAt> Ext4Ro<R> {
    /// Open the filesystem that occupies `len` bytes at `offset` in `reader`.
    pub(crate) fn open(reader: R, offset: u64, len: u64) -> Result<Self, Ext4Error> {
        let mut fs = Self {
            reader,
            offset,
            len,
            fs_bytes: len,
            block_size: 1024,
            inode_size: 128,
            inodes_per_group: 1,
            inodes_count: 1,
            blocks_count: 1,
            inode_tables: Vec::new(),
            filetype: false,
        };
        // Superblock lives at byte 1024 and the 64-bit fields end by 0x154.
        const SB_NEED: u64 = 0x154;
        if len < 1024 + SB_NEED {
            return Err(Ext4Error::Corrupt("filesystem is too small"));
        }
        let mut sb = vec![0u8; SB_NEED as usize];
        fs.read_absolute(offset + 1024, &mut sb)?;
        let geometry = parse_superblock(&sb, len)?;
        fs.fs_bytes = geometry.fs_bytes;
        fs.block_size = geometry.block_size;
        fs.inode_size = geometry.inode_size;
        fs.inodes_per_group = geometry.inodes_per_group;
        fs.inodes_count = geometry.inodes_count;
        fs.blocks_count = geometry.blocks_count;
        fs.filetype = geometry.filetype;
        fs.inode_tables = fs.read_inode_tables(&geometry)?;
        Ok(fs)
    }

    /// Read the inode table block of every group from the group descriptor
    /// table. `self` must already carry the sizes from `geometry`, because
    /// descriptor reads are bounded by `fs_bytes`.
    fn read_inode_tables(&self, geometry: &Geometry) -> Result<Vec<u64>, Ext4Error> {
        // The group descriptor table starts in the block after the one that
        // holds the superblock, that is at block `first_data_block + 1`.
        let gdt = (u64::from(geometry.first_data_block) + 1)
            .checked_mul(geometry.block_size)
            .ok_or(Ext4Error::Corrupt("size overflow"))?;
        let table_blocks = div_ceil(
            u64::from(geometry.inodes_per_group)
                .checked_mul(geometry.inode_size)
                .ok_or(Ext4Error::Corrupt("size overflow"))?,
            geometry.block_size,
        )?;
        let mut inode_tables = Vec::with_capacity(geometry.groups as usize);
        for group in 0..geometry.groups {
            let desc_off = gdt
                .checked_add(
                    group
                        .checked_mul(geometry.desc_size)
                        .ok_or(Ext4Error::Corrupt("size overflow"))?,
                )
                .ok_or(Ext4Error::Corrupt("size overflow"))?;
            let mut desc = vec![0u8; geometry.desc_size as usize];
            self.read_at_fs(desc_off, &mut desc)?;
            // 0x08 bg_inode_table_lo, 0x28 bg_inode_table_hi. The high half
            // exists only in descriptors of at least 0x2C bytes.
            let lo = u32_at(&desc, 0x08)? as u64;
            let hi = if geometry.desc_size >= 0x2C {
                u32_at(&desc, 0x28)? as u64
            } else {
                0
            };
            let table = lo | (hi << 32);
            let table_end = table
                .checked_add(table_blocks)
                .ok_or(Ext4Error::Corrupt("size overflow"))?;
            if table == 0 || table_end > geometry.blocks_count {
                return Err(Ext4Error::Corrupt("inode table is out of range"));
            }
            inode_tables.push(table);
        }
        Ok(inode_tables)
    }

    /// Read `path` without following symlinks.
    ///
    /// A symlink at the final component is `FileRead::Symlink`. A symlink in
    /// an earlier component is an error: callers that want one level of
    /// indirection (only `/etc/os-release`) resolve the returned target
    /// themselves. `max_len` is a hard cap; the file is not truncated.
    pub(crate) fn read_file(&self, path: &str, max_len: usize) -> Result<FileRead, Ext4Error> {
        match self.walk(path)? {
            Walked::Root => Ok(FileRead::Other),
            Walked::Missing => Ok(FileRead::Missing),
            Walked::Found(ino) => self.read_inode_data(ino, max_len),
        }
    }

    /// Metadata of the final component. A final symlink is not followed.
    /// `Ok(None)` means the path is missing.
    pub(crate) fn lstat(&self, path: &str) -> Result<Option<FileMeta>, Ext4Error> {
        match self.walk(path)? {
            Walked::Root | Walked::Missing => Ok(None),
            Walked::Found(ino) => {
                let inode = self.read_inode(ino)?;
                Ok(Some(FileMeta {
                    mode: inode.mode,
                    uid: inode.uid,
                    gid: inode.gid,
                }))
            }
        }
    }

    fn walk(&self, path: &str) -> Result<Walked, Ext4Error> {
        if !path.starts_with('/') {
            return Err(Ext4Error::Corrupt("path is not absolute"));
        }
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        if parts.len() > MAX_PATH_COMPONENTS {
            return Err(Ext4Error::Unsupported("path is too deep"));
        }
        if parts.iter().any(|part| *part == "." || *part == "..") {
            return Err(Ext4Error::Unsupported("path contains . or .."));
        }
        if parts.is_empty() {
            return Ok(Walked::Root);
        }
        let mut ino = 2u32;
        for (index, part) in parts.iter().enumerate() {
            let dir = self.read_inode(ino)?;
            if dir.mode & S_IFMT != S_IFDIR {
                return Err(Ext4Error::Unsupported("path component is not a directory"));
            }
            let Some(child) = self.lookup(&dir, part.as_bytes())? else {
                return Ok(Walked::Missing);
            };
            if index + 1 == parts.len() {
                return Ok(Walked::Found(child));
            }
            let next = self.read_inode(child)?;
            match next.mode & S_IFMT {
                S_IFLNK => return Err(Ext4Error::Unsupported("symlink in path")),
                S_IFDIR => ino = child,
                _ => return Err(Ext4Error::Unsupported("path component is not a directory")),
            }
        }
        Ok(Walked::Missing)
    }

    fn read_inode_data(&self, ino: u32, max_len: usize) -> Result<FileRead, Ext4Error> {
        let inode = self.read_inode(ino)?;
        match inode.mode & S_IFMT {
            S_IFLNK => Ok(FileRead::Symlink(self.read_symlink(&inode)?)),
            S_IFREG => Ok(FileRead::Regular(self.read_regular(&inode, max_len)?)),
            _ => Ok(FileRead::Other),
        }
    }

    fn read_symlink(&self, inode: &Inode) -> Result<String, Ext4Error> {
        let size = u64::from(inode.size_lo);
        if size > MAX_SYMLINK_LEN {
            return Err(Ext4Error::Corrupt("symlink too long"));
        }
        // Fast symlinks live in i_block. Do not parse those bytes as extents,
        // even when they happen to start with the extent magic.
        let bytes = if size < 60 {
            inode.iblock[..size as usize].to_vec()
        } else {
            self.read_mapped(inode, size, size as usize)?
        };
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn read_regular(&self, inode: &Inode, max_len: usize) -> Result<Vec<u8>, Ext4Error> {
        if inode.size > max_len as u64 {
            return Err(Ext4Error::TooLarge);
        }
        if inode.size == 0 {
            return Ok(Vec::new());
        }
        self.read_mapped(inode, inode.size, max_len)
    }

    fn read_block(&self, block: u64, buf: &mut [u8]) -> Result<(), Ext4Error> {
        if block >= self.blocks_count || buf.len() as u64 != self.block_size {
            return Err(Ext4Error::Corrupt("block out of range"));
        }
        let off = block
            .checked_mul(self.block_size)
            .ok_or(Ext4Error::Corrupt("size overflow"))?;
        self.read_at_fs(off, buf)
    }

    fn read_at_fs(&self, fs_off: u64, buf: &mut [u8]) -> Result<(), Ext4Error> {
        let end = fs_off
            .checked_add(buf.len() as u64)
            .ok_or(Ext4Error::Corrupt("size overflow"))?;
        if end > self.fs_bytes || end > self.len {
            return Err(Ext4Error::Corrupt("read out of range"));
        }
        let abs = self
            .offset
            .checked_add(fs_off)
            .ok_or(Ext4Error::Corrupt("size overflow"))?;
        self.read_absolute(abs, buf)
    }

    fn read_absolute(&self, offset: u64, buf: &mut [u8]) -> Result<(), Ext4Error> {
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = self
                .reader
                .read_at(&mut buf[filled..], offset + filled as u64)
                .map_err(Ext4Error::Io)?;
            if n == 0 {
                return Err(Ext4Error::Io(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "short read",
                )));
            }
            filled += n;
        }
        Ok(())
    }
}

/// Filesystem layout values from the superblock, checked for consistency.
struct Geometry {
    /// `blocks_count * block_size`, never more than the partition length.
    fs_bytes: u64,
    block_size: u64,
    inode_size: u64,
    inodes_per_group: u32,
    inodes_count: u32,
    blocks_count: u64,
    /// Number of block groups, equal to the number of group descriptors.
    groups: u64,
    desc_size: u64,
    first_data_block: u32,
    filetype: bool,
}

/// Parse and validate the superblock `sb`, read from a partition of `len`
/// bytes.
fn parse_superblock(sb: &[u8], len: u64) -> Result<Geometry, Ext4Error> {
    // 0x38 s_magic
    if u16_at(sb, 0x38)? != EXT4_MAGIC {
        return Err(Ext4Error::Corrupt("bad superblock magic"));
    }
    // 0x4C s_rev_level. Revision 0 has no feature fields and a fixed
    // 128-byte inode; 0x60 s_feature_incompat exists from revision 1.
    let rev = u32_at(sb, 0x4C)?;
    let incompat = if rev == 0 { 0 } else { u32_at(sb, 0x60)? };
    if incompat & INCOMPAT_RECOVER != 0 {
        return Err(Ext4Error::Unsupported("needs_recovery"));
    }
    if incompat & !INCOMPAT_ALLOWED != 0 {
        return Err(Ext4Error::Unsupported("unsupported incompat features"));
    }
    // 0x18 s_log_block_size: block size is 1024 << value (up to 64 KiB).
    let log_block = u32_at(sb, 0x18)?;
    if log_block > 6 {
        return Err(Ext4Error::Unsupported("unsupported block size"));
    }
    let block_size = 1024u64 << log_block;
    // 0x04 s_blocks_count_lo, 0x150 s_blocks_count_hi (64bit feature only).
    let blocks_lo = u32_at(sb, 0x04)? as u64;
    let blocks_hi = if incompat & INCOMPAT_64BIT != 0 {
        u32_at(sb, 0x150)? as u64
    } else {
        0
    };
    let blocks_count = blocks_lo | (blocks_hi << 32);
    if blocks_count == 0 {
        return Err(Ext4Error::Corrupt("filesystem has no blocks"));
    }
    let fs_bytes = blocks_count
        .checked_mul(block_size)
        .ok_or(Ext4Error::Corrupt("size overflow"))?;
    if fs_bytes > len {
        return Err(Ext4Error::Corrupt("filesystem larger than the partition"));
    }
    let inode_size = if rev == 0 {
        128u64
    } else {
        // 0x58 s_inode_size
        u64::from(u16_at(sb, 0x58)?)
    };
    if inode_size < 128 || inode_size > block_size {
        return Err(Ext4Error::Unsupported("unsupported inode size"));
    }
    // 0x00 s_inodes_count, 0x28 s_inodes_per_group, 0x20 s_blocks_per_group
    let inodes_count = u32_at(sb, 0x00)?;
    let inodes_per_group = u32_at(sb, 0x28)?;
    let blocks_per_group = u32_at(sb, 0x20)?;
    if inodes_count == 0 || inodes_per_group == 0 || blocks_per_group == 0 {
        return Err(Ext4Error::Corrupt("invalid group size"));
    }
    // Every group has one descriptor and one inode table, so counting the
    // groups by blocks and by inodes must agree. A mismatch means the
    // superblock is corrupt, and either count could index past the
    // descriptor table.
    let groups_b = div_ceil(blocks_count, u64::from(blocks_per_group))?;
    let groups_i = div_ceil(u64::from(inodes_count), u64::from(inodes_per_group))?;
    if groups_b != groups_i || groups_b == 0 || groups_b > MAX_GROUPS {
        return Err(Ext4Error::Corrupt("invalid group count"));
    }
    let desc_size: u64 = if incompat & INCOMPAT_64BIT != 0 {
        // 0xFE s_desc_size; without the 64bit feature descriptors are 32 bytes.
        let desc = u16_at(sb, 0xFE)?;
        if !(64..=1024).contains(&desc) || !desc.is_multiple_of(8) {
            return Err(Ext4Error::Unsupported("group descriptor size"));
        }
        u64::from(desc)
    } else {
        32
    };
    // 0x14 s_first_data_block: the block that holds the superblock. It is 1
    // for 1 KiB blocks, where block 0 is the boot sector, and 0 otherwise.
    let first_data = u32_at(sb, 0x14)?;
    if u64::from(first_data) >= blocks_count {
        return Err(Ext4Error::Corrupt("first data block is out of range"));
    }
    Ok(Geometry {
        fs_bytes,
        block_size,
        inode_size,
        inodes_per_group,
        inodes_count,
        blocks_count,
        groups: groups_b,
        desc_size,
        first_data_block: first_data,
        filetype: incompat & INCOMPAT_FILETYPE != 0,
    })
}

fn div_ceil(n: u64, d: u64) -> Result<u64, Ext4Error> {
    if d == 0 {
        return Err(Ext4Error::Corrupt("size overflow"));
    }
    n.checked_add(d - 1)
        .and_then(|sum| sum.checked_div(d))
        .ok_or(Ext4Error::Corrupt("size overflow"))
}

fn u16_at(buf: &[u8], off: usize) -> Result<u16, Ext4Error> {
    let bytes = buf
        .get(off..off + 2)
        .ok_or(Ext4Error::Corrupt("truncated field"))?;
    let mut raw = [0u8; 2];
    raw.copy_from_slice(bytes);
    Ok(u16::from_le_bytes(raw))
}

fn u32_at(buf: &[u8], off: usize) -> Result<u32, Ext4Error> {
    let bytes = buf
        .get(off..off + 4)
        .ok_or(Ext4Error::Corrupt("truncated field"))?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(raw))
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::{gunzip_ext4_fixture, sample_ext4_image};

#[cfg(test)]
mod tests;
