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

use std::{
    fs::File,
    io::{self, ErrorKind},
    os::unix::fs::FileExt,
};

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
const MAX_GROUPS: u64 = 100_000;
const MAX_EXTENT_DEPTH: u16 = 5;
const MAX_EXTENT_NODES: usize = 64;
const MAX_EXTENTS: usize = 4096;
const MAX_SYMLINK_LEN: u64 = 4096;
/// Stop a directory walk after this many bytes so a hostile tree cannot loop.
const DIR_SCAN_LIMIT: u64 = 4 * 1024 * 1024;
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

struct Inode {
    mode: u16,
    uid: u32,
    gid: u32,
    flags: u32,
    /// Size used for regular files and directories (includes `i_size_high`).
    size: u64,
    /// Low 32 bits, which is the whole size of a symlink.
    size_lo: u32,
    iblock: [u8; 60],
}

/// `lstat` of one path. The final symlink is not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileMeta {
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
}

struct Extent {
    logical: u32,
    len: u32,
    start: u64,
    uninit: bool,
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

        if u16_at(&sb, 0x38)? != EXT4_MAGIC {
            return Err(Ext4Error::Corrupt("bad superblock magic"));
        }
        let rev = u32_at(&sb, 0x4C)?;
        let incompat = if rev == 0 { 0 } else { u32_at(&sb, 0x60)? };
        if incompat & INCOMPAT_RECOVER != 0 {
            return Err(Ext4Error::Unsupported("needs_recovery"));
        }
        if incompat & !INCOMPAT_ALLOWED != 0 {
            return Err(Ext4Error::Unsupported("unsupported incompat features"));
        }
        let log_block = u32_at(&sb, 0x18)?;
        if log_block > 6 {
            return Err(Ext4Error::Unsupported("unsupported block size"));
        }
        let block_size = 1024u64 << log_block;
        let blocks_lo = u32_at(&sb, 0x04)? as u64;
        let blocks_hi = if incompat & INCOMPAT_64BIT != 0 {
            u32_at(&sb, 0x150)? as u64
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
            u64::from(u16_at(&sb, 0x58)?)
        };
        if inode_size < 128 || inode_size > block_size {
            return Err(Ext4Error::Unsupported("unsupported inode size"));
        }
        let inodes_count = u32_at(&sb, 0x00)?;
        let inodes_per_group = u32_at(&sb, 0x28)?;
        let blocks_per_group = u32_at(&sb, 0x20)?;
        if inodes_count == 0 || inodes_per_group == 0 || blocks_per_group == 0 {
            return Err(Ext4Error::Corrupt("invalid group size"));
        }
        let groups_b = div_ceil(blocks_count, u64::from(blocks_per_group))?;
        let groups_i = div_ceil(u64::from(inodes_count), u64::from(inodes_per_group))?;
        if groups_b != groups_i || groups_b == 0 || groups_b > MAX_GROUPS {
            return Err(Ext4Error::Corrupt("invalid group count"));
        }
        let desc_size: u64 = if incompat & INCOMPAT_64BIT != 0 {
            let desc = u16_at(&sb, 0xFE)?;
            if !(64..=1024).contains(&desc) || !desc.is_multiple_of(8) {
                return Err(Ext4Error::Unsupported("group descriptor size"));
            }
            u64::from(desc)
        } else {
            32
        };
        let first_data = u32_at(&sb, 0x14)?;
        if u64::from(first_data) >= blocks_count {
            return Err(Ext4Error::Corrupt("first data block is out of range"));
        }
        fs.fs_bytes = fs_bytes;
        fs.block_size = block_size;
        fs.inode_size = inode_size;
        fs.inodes_per_group = inodes_per_group;
        fs.inodes_count = inodes_count;
        fs.blocks_count = blocks_count;
        fs.filetype = incompat & INCOMPAT_FILETYPE != 0;

        let gdt = (u64::from(first_data) + 1)
            .checked_mul(block_size)
            .ok_or(Ext4Error::Corrupt("size overflow"))?;
        let table_blocks = div_ceil(
            u64::from(inodes_per_group)
                .checked_mul(inode_size)
                .ok_or(Ext4Error::Corrupt("size overflow"))?,
            block_size,
        )?;
        let mut inode_tables = Vec::with_capacity(groups_b as usize);
        for group in 0..groups_b {
            let desc_off = gdt
                .checked_add(
                    group
                        .checked_mul(desc_size)
                        .ok_or(Ext4Error::Corrupt("size overflow"))?,
                )
                .ok_or(Ext4Error::Corrupt("size overflow"))?;
            let mut desc = vec![0u8; desc_size as usize];
            fs.read_at_fs(desc_off, &mut desc)?;
            let lo = u32_at(&desc, 0x08)? as u64;
            let hi = if desc_size >= 0x2C {
                u32_at(&desc, 0x28)? as u64
            } else {
                0
            };
            let table = lo | (hi << 32);
            let table_end = table
                .checked_add(table_blocks)
                .ok_or(Ext4Error::Corrupt("size overflow"))?;
            if table == 0 || table_end > blocks_count {
                return Err(Ext4Error::Corrupt("inode table is out of range"));
            }
            inode_tables.push(table);
        }
        fs.inode_tables = inode_tables;
        Ok(fs)
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

    fn read_mapped(&self, inode: &Inode, size: u64, max_len: usize) -> Result<Vec<u8>, Ext4Error> {
        if inode.flags & EXTENTS_FL == 0 {
            return Err(Ext4Error::Unsupported("inode is not extent-mapped"));
        }
        if size > max_len as u64 {
            return Err(Ext4Error::TooLarge);
        }
        let extents = self.load_extents(&inode.iblock)?;
        let mut out = vec![0u8; size as usize];
        for extent in &extents {
            let start = u64::from(extent.logical)
                .checked_mul(self.block_size)
                .ok_or(Ext4Error::Corrupt("size overflow"))?;
            if start >= size {
                continue;
            }
            let extent_bytes = u64::from(extent.len)
                .checked_mul(self.block_size)
                .ok_or(Ext4Error::Corrupt("size overflow"))?;
            let copy = extent_bytes.min(size - start);
            if extent.uninit {
                continue;
            }
            let mut done = 0u64;
            while done < copy {
                let block_index = done / self.block_size;
                let block_off = done % self.block_size;
                let phys = extent
                    .start
                    .checked_add(block_index)
                    .ok_or(Ext4Error::Corrupt("size overflow"))?;
                let mut block = vec![0u8; self.block_size as usize];
                self.read_block(phys, &mut block)?;
                let n = ((self.block_size - block_off).min(copy - done)) as usize;
                let dst = usize::try_from(start + done)
                    .map_err(|_| Ext4Error::Corrupt("size overflow"))?;
                let src = block_off as usize;
                out[dst..dst + n].copy_from_slice(&block[src..src + n]);
                done += n as u64;
            }
        }
        Ok(out)
    }

    fn load_extents(&self, iblock: &[u8; 60]) -> Result<Vec<Extent>, Ext4Error> {
        let mut nodes = 0usize;
        let mut extents = Vec::new();
        self.walk_extent_node(iblock, &mut nodes, &mut extents)?;
        extents.sort_by_key(|extent| (extent.logical, extent.len));
        for pair in extents.windows(2) {
            let end = u64::from(pair[0].logical) + u64::from(pair[0].len);
            if end > u64::from(pair[1].logical) {
                return Err(Ext4Error::Corrupt("overlapping extents"));
            }
        }
        Ok(extents)
    }

    fn walk_extent_node(
        &self,
        data: &[u8],
        nodes: &mut usize,
        extents: &mut Vec<Extent>,
    ) -> Result<(), Ext4Error> {
        *nodes += 1;
        if *nodes > MAX_EXTENT_NODES {
            return Err(Ext4Error::Corrupt("extent tree is too wide"));
        }
        if data.len() < 12 {
            return Err(Ext4Error::Corrupt("truncated extent header"));
        }
        if u16_at(data, 0)? != EXTENT_MAGIC {
            return Err(Ext4Error::Corrupt("bad extent magic"));
        }
        let entries = u16_at(data, 2)? as usize;
        let max = u16_at(data, 4)? as usize;
        let depth = u16_at(data, 6)?;
        if depth > MAX_EXTENT_DEPTH {
            return Err(Ext4Error::Corrupt("extent depth"));
        }
        if entries > max {
            return Err(Ext4Error::Corrupt("extent header is inconsistent"));
        }
        let need = 12usize
            .checked_add(
                entries
                    .checked_mul(12)
                    .ok_or(Ext4Error::Corrupt("size overflow"))?,
            )
            .ok_or(Ext4Error::Corrupt("size overflow"))?;
        if need > data.len() {
            return Err(Ext4Error::Corrupt("extent entries do not fit"));
        }
        if depth == 0 {
            for index in 0..entries {
                let off = 12 + index * 12;
                let logical = u32_at(data, off)?;
                let ee_len = u16_at(data, off + 4)?;
                let len = u32::from(ee_len & 0x7FFF);
                if len == 0 {
                    return Err(Ext4Error::Corrupt("extent length is zero"));
                }
                let start_hi = u16_at(data, off + 6)? as u64;
                let start_lo = u32_at(data, off + 8)? as u64;
                let start = start_lo | (start_hi << 32);
                let end = start
                    .checked_add(u64::from(len))
                    .ok_or(Ext4Error::Corrupt("size overflow"))?;
                if start == 0 || end > self.blocks_count {
                    return Err(Ext4Error::Corrupt("extent block out of range"));
                }
                let logical_end = u64::from(logical)
                    .checked_add(u64::from(len))
                    .ok_or(Ext4Error::Corrupt("size overflow"))?;
                if logical_end > u64::from(u32::MAX) {
                    return Err(Ext4Error::Corrupt("extent logical block overflow"));
                }
                extents.push(Extent {
                    logical,
                    len,
                    start,
                    uninit: ee_len & 0x8000 != 0,
                });
                if extents.len() > MAX_EXTENTS {
                    return Err(Ext4Error::Corrupt("too many extents"));
                }
            }
            return Ok(());
        }
        for index in 0..entries {
            let off = 12 + index * 12;
            let leaf_lo = u32_at(data, off + 4)? as u64;
            let leaf_hi = u16_at(data, off + 8)? as u64;
            let leaf = leaf_lo | (leaf_hi << 32);
            if leaf == 0 || leaf >= self.blocks_count {
                return Err(Ext4Error::Corrupt("extent index block out of range"));
            }
            let mut block = vec![0u8; self.block_size as usize];
            self.read_block(leaf, &mut block)?;
            if block.len() < 12 {
                return Err(Ext4Error::Corrupt("truncated extent header"));
            }
            let child_depth = u16_at(&block, 6)?;
            if child_depth != depth - 1 {
                return Err(Ext4Error::Corrupt("extent depth"));
            }
            self.walk_extent_node(&block, nodes, extents)?;
        }
        Ok(())
    }

    fn lookup(&self, dir: &Inode, name: &[u8]) -> Result<Option<u32>, Ext4Error> {
        if name.len() > 255 || dir.size == 0 {
            return Ok(None);
        }
        if dir.flags & EXTENTS_FL == 0 {
            return Err(Ext4Error::Unsupported("inode is not extent-mapped"));
        }
        let extents = self.load_extents(&dir.iblock)?;
        let blocks = div_ceil(dir.size, self.block_size)?;
        let max_blocks = DIR_SCAN_LIMIT / self.block_size;
        let scan = blocks.min(max_blocks);
        for logical in 0..scan {
            let mut block = vec![0u8; self.block_size as usize];
            if let Some((phys, uninit)) = self.phys_for(&extents, logical)?
                && !uninit
            {
                self.read_block(phys, &mut block)?;
            }
            let valid = if logical + 1 == blocks {
                (dir.size - logical * self.block_size) as usize
            } else {
                self.block_size as usize
            };
            if let Some(ino) = self.parse_dir_block(&block[..valid], name)? {
                return Ok(Some(ino));
            }
        }
        if blocks > max_blocks {
            return Err(Ext4Error::Unsupported("directory too large"));
        }
        Ok(None)
    }

    fn phys_for(&self, extents: &[Extent], logical: u64) -> Result<Option<(u64, bool)>, Ext4Error> {
        for extent in extents {
            let start = u64::from(extent.logical);
            let end = start + u64::from(extent.len);
            if logical >= start && logical < end {
                let phys = extent
                    .start
                    .checked_add(logical - start)
                    .ok_or(Ext4Error::Corrupt("size overflow"))?;
                return Ok(Some((phys, extent.uninit)));
            }
        }
        Ok(None)
    }

    fn parse_dir_block(&self, block: &[u8], name: &[u8]) -> Result<Option<u32>, Ext4Error> {
        let mut off = 0usize;
        while off + 8 <= block.len() {
            let ino = u32_at(block, off)?;
            let rec_len = u16_at(block, off + 4)? as usize;
            if rec_len == 0 {
                return Err(Ext4Error::Corrupt("directory entry length"));
            }
            if rec_len < 8 || !rec_len.is_multiple_of(4) || off + rec_len > block.len() {
                return Err(Ext4Error::Corrupt("directory entry length"));
            }
            if ino != 0 {
                let (name_len, name_off) = if self.filetype {
                    (block[off + 6] as usize, off + 8)
                } else {
                    (u16_at(block, off + 6)? as usize, off + 8)
                };
                if name_off + name_len > off + rec_len {
                    return Err(Ext4Error::Corrupt("directory name does not fit"));
                }
                if &block[name_off..name_off + name_len] == name {
                    return Ok(Some(ino));
                }
            }
            off += rec_len;
        }
        Ok(None)
    }

    fn read_inode(&self, ino: u32) -> Result<Inode, Ext4Error> {
        if ino == 0 || ino > self.inodes_count {
            return Err(Ext4Error::Corrupt("inode number is out of range"));
        }
        let group = (ino - 1) / self.inodes_per_group;
        let index = (ino - 1) % self.inodes_per_group;
        let table = self
            .inode_tables
            .get(group as usize)
            .copied()
            .ok_or(Ext4Error::Corrupt("inode group is out of range"))?;
        let off = table
            .checked_mul(self.block_size)
            .and_then(|base| base.checked_add(u64::from(index) * self.inode_size))
            .ok_or(Ext4Error::Corrupt("size overflow"))?;
        let mut raw = vec![0u8; self.inode_size as usize];
        self.read_at_fs(off, &mut raw)?;
        if raw.len() < 0x28 + 60 {
            return Err(Ext4Error::Corrupt("inode is truncated"));
        }
        let size_lo = u32_at(&raw, 0x04)?;
        let size_hi = if self.inode_size >= 0x70 {
            u32_at(&raw, 0x6C)?
        } else {
            0
        };
        let uid_lo = u32::from(u16_at(&raw, 0x02)?);
        let gid_lo = u32::from(u16_at(&raw, 0x18)?);
        let (uid_hi, gid_hi) = if raw.len() >= 0x7C {
            (
                u32::from(u16_at(&raw, 0x78)?),
                u32::from(u16_at(&raw, 0x7A)?),
            )
        } else {
            (0, 0)
        };
        let mut iblock = [0u8; 60];
        iblock.copy_from_slice(&raw[0x28..0x28 + 60]);
        Ok(Inode {
            mode: u16_at(&raw, 0x00)?,
            uid: uid_lo | (uid_hi << 16),
            gid: gid_lo | (gid_hi << 16),
            flags: u32_at(&raw, 0x20)?,
            size: u64::from(size_lo) | (u64::from(size_hi) << 32),
            size_lo,
            iblock,
        })
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
pub(crate) struct MemReader<'a>(pub &'a [u8]);

#[cfg(test)]
impl ReadAt for MemReader<'_> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let start = usize::try_from(offset)
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "offset"))?;
        if start >= self.0.len() {
            return Err(io::Error::new(ErrorKind::UnexpectedEof, "eof"));
        }
        let n = buf.len().min(self.0.len() - start);
        buf[..n].copy_from_slice(&self.0[start..start + n]);
        Ok(n)
    }
}

/// Minimal extent-mapped ext4 used by in-memory tests. Layout (4 KiB blocks):
/// block 0 superblock, 1 group descriptor, 4 inode table, 5 root directory,
/// 6 `hello`, 7 long symlink, 8–9 hole file, 10 extent index, 11 `deep`.
#[cfg(test)]
pub(crate) fn sample_ext4_image() -> Vec<u8> {
    const BS: usize = 4096;
    const BLOCKS: usize = 16;
    const INODE_SIZE: usize = 256;
    let mut img = vec![0u8; BLOCKS * BS];
    put_u32(&mut img, 1024, 16); // s_inodes_count at superblock + 0x00
    put_u32(&mut img, 1024 + 0x04, BLOCKS as u32);
    put_u32(&mut img, 1024 + 0x14, 0);
    put_u32(&mut img, 1024 + 0x18, 2);
    put_u32(&mut img, 1024 + 0x20, BLOCKS as u32);
    put_u32(&mut img, 1024 + 0x28, 16);
    put_u16(&mut img, 1024 + 0x38, EXT4_MAGIC);
    put_u32(&mut img, 1024 + 0x4C, 1);
    put_u16(&mut img, 1024 + 0x58, INODE_SIZE as u16);
    put_u32(&mut img, 1024 + 0x60, INCOMPAT_FILETYPE | INCOMPAT_EXTENTS);
    put_u32(&mut img, BS + 0x08, 4);

    write_extent_inode(&mut img, 2, S_IFDIR | 0o755, BS as u64, &[(0, 1, 5)]);
    let hello = b"hello-ext4\n";
    img[6 * BS..6 * BS + hello.len()].copy_from_slice(hello);
    write_extent_inode(
        &mut img,
        3,
        S_IFREG | 0o644,
        hello.len() as u64,
        &[(0, 1, 6)],
    );
    write_fast_symlink(&mut img, 4, b"fast-target");
    let long = vec![b'L'; 80];
    img[7 * BS..7 * BS + long.len()].copy_from_slice(&long);
    write_extent_inode(
        &mut img,
        5,
        S_IFLNK | 0o777,
        long.len() as u64,
        &[(0, 1, 7)],
    );
    img[8 * BS..8 * BS + 3].copy_from_slice(b"AAA");
    img[9 * BS..9 * BS + 3].copy_from_slice(b"CCC");
    write_extent_inode(
        &mut img,
        6,
        S_IFREG | 0o644,
        3 * BS as u64,
        &[(0, 1, 8), (2, 1, 9)],
    );
    // Depth-1 extent tree: inode index -> block 10 -> leaf extent at block 11.
    write_index_inode(&mut img, 7, S_IFREG | 0o644, 7, 10);
    img[11 * BS..11 * BS + 7].copy_from_slice(b"deep-ok");
    write_leaf_block(&mut img, 10, 0, 1, 11);
    write_fast_symlink(&mut img, 8, b"hello");

    let entries = [
        DirEnt {
            ino: 2,
            name: ".",
            ty: 2,
        },
        DirEnt {
            ino: 2,
            name: "..",
            ty: 2,
        },
        DirEnt {
            ino: 3,
            name: "hello",
            ty: 1,
        },
        DirEnt {
            ino: 4,
            name: "short",
            ty: 7,
        },
        DirEnt {
            ino: 5,
            name: "long",
            ty: 7,
        },
        DirEnt {
            ino: 6,
            name: "hole",
            ty: 1,
        },
        DirEnt {
            ino: 7,
            name: "deep",
            ty: 1,
        },
        DirEnt {
            ino: 8,
            name: "via",
            ty: 7,
        },
    ];
    write_dir_block(&mut img[5 * BS..6 * BS], &entries);
    img
}

#[cfg(test)]
struct DirEnt {
    ino: u32,
    name: &'static str,
    ty: u8,
}

#[cfg(test)]
fn sample_inode_offset(ino: u32) -> usize {
    4 * 4096 + (ino as usize - 1) * 256
}

#[cfg(test)]
fn write_extent_inode(img: &mut [u8], ino: u32, mode: u16, size: u64, extents: &[(u32, u16, u64)]) {
    let off = sample_inode_offset(ino);
    put_u16(img, off, mode);
    put_u32(img, off + 0x04, size as u32);
    put_u32(img, off + 0x6C, (size >> 32) as u32);
    put_u32(img, off + 0x20, EXTENTS_FL);
    write_extent_header(&mut img[off + 0x28..off + 0x28 + 60], extents);
}

#[cfg(test)]
fn write_index_inode(img: &mut [u8], ino: u32, mode: u16, size: u64, leaf: u64) {
    let off = sample_inode_offset(ino);
    put_u16(img, off, mode);
    put_u32(img, off + 0x04, size as u32);
    put_u32(img, off + 0x20, EXTENTS_FL);
    let ib = &mut img[off + 0x28..off + 0x28 + 60];
    put_u16(ib, 0, EXTENT_MAGIC);
    put_u16(ib, 2, 1);
    put_u16(ib, 4, 4);
    put_u16(ib, 6, 1);
    put_u32(ib, 12, 0);
    put_u32(ib, 16, leaf as u32);
    put_u16(ib, 20, (leaf >> 32) as u16);
}

#[cfg(test)]
fn write_leaf_block(img: &mut [u8], block: usize, logical: u32, len: u16, phys: u64) {
    let off = block * 4096;
    put_u16(img, off, EXTENT_MAGIC);
    put_u16(img, off + 2, 1);
    put_u16(img, off + 4, 340);
    put_u16(img, off + 6, 0);
    put_u32(img, off + 12, logical);
    put_u16(img, off + 16, len);
    put_u16(img, off + 18, (phys >> 32) as u16);
    put_u32(img, off + 20, phys as u32);
}

#[cfg(test)]
fn write_extent_header(iblock: &mut [u8], extents: &[(u32, u16, u64)]) {
    put_u16(iblock, 0, EXTENT_MAGIC);
    put_u16(iblock, 2, extents.len() as u16);
    put_u16(iblock, 4, 4);
    put_u16(iblock, 6, 0);
    for (index, (logical, len, phys)) in extents.iter().enumerate() {
        let off = 12 + index * 12;
        put_u32(iblock, off, *logical);
        put_u16(iblock, off + 4, *len);
        put_u16(iblock, off + 6, (*phys >> 32) as u16);
        put_u32(iblock, off + 8, *phys as u32);
    }
}

#[cfg(test)]
fn write_fast_symlink(img: &mut [u8], ino: u32, target: &[u8]) {
    assert!(target.len() < 60);
    let off = sample_inode_offset(ino);
    put_u16(img, off, S_IFLNK | 0o777);
    put_u32(img, off + 0x04, target.len() as u32);
    img[off + 0x28..off + 0x28 + target.len()].copy_from_slice(target);
}

#[cfg(test)]
fn write_dir_block(block: &mut [u8], entries: &[DirEnt]) {
    let mut off = 0usize;
    for (index, entry) in entries.iter().enumerate() {
        let min = (8 + entry.name.len()).div_ceil(4) * 4;
        let rec = if index + 1 == entries.len() {
            block.len() - off
        } else {
            min
        };
        assert!(rec >= min && rec % 4 == 0 && off + rec <= block.len());
        put_u32(block, off, entry.ino);
        put_u16(block, off + 4, rec as u16);
        block[off + 6] = entry.name.len() as u8;
        block[off + 7] = entry.ty;
        block[off + 8..off + 8 + entry.name.len()].copy_from_slice(entry.name.as_bytes());
        off += rec;
    }
    assert_eq!(off, block.len());
}

#[cfg(test)]
fn put_u16(buf: &mut [u8], off: usize, value: u16) {
    buf[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
fn put_u32(buf: &mut [u8], off: usize, value: u32) {
    buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
fn open_sample(img: &[u8]) -> Ext4Ro<MemReader<'_>> {
    Ext4Ro::open(MemReader(img), 0, img.len() as u64).unwrap()
}

#[cfg(test)]
fn gunzip_fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let output = std::process::Command::new("/usr/bin/gzip")
        .arg("-dc")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "gzip {}: {}",
        name,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_regular_symlink_hole_and_extent_index() {
        let img = sample_ext4_image();
        let fs = open_sample(&img);
        assert_eq!(
            fs.read_file("/hello", 64).unwrap(),
            FileRead::Regular(b"hello-ext4\n".to_vec())
        );
        assert_eq!(
            fs.read_file("/short", 64).unwrap(),
            FileRead::Symlink("fast-target".into())
        );
        assert_eq!(
            fs.read_file("/long", 128).unwrap(),
            FileRead::Symlink("L".repeat(80))
        );
        match fs.read_file("/hole", 64 * 1024).unwrap() {
            FileRead::Regular(bytes) => {
                assert_eq!(&bytes[..3], b"AAA");
                assert!(bytes[4096..4099].iter().all(|b| *b == 0));
                assert_eq!(&bytes[8192..8195], b"CCC");
                assert_eq!(bytes.len(), 12288);
            }
            other => panic!("hole file: {other:?}"),
        }
        assert_eq!(
            fs.read_file("/deep", 64).unwrap(),
            FileRead::Regular(b"deep-ok".to_vec())
        );
        assert_eq!(fs.read_file("/missing", 64).unwrap(), FileRead::Missing);
        let mut owned = img.clone();
        let hello = sample_inode_offset(3);
        put_u16(&mut owned, hello + 0x02, 7);
        put_u16(&mut owned, hello + 0x18, 42);
        put_u16(&mut owned, hello + 0x78, 1);
        put_u16(&mut owned, hello + 0x7A, 2);
        let fs = open_sample(&owned);
        assert_eq!(
            fs.lstat("/hello").unwrap(),
            Some(FileMeta {
                mode: S_IFREG | 0o644,
                uid: (1 << 16) | 7,
                gid: (2 << 16) | 42,
            })
        );
        let link = fs.lstat("/short").unwrap().unwrap();
        assert_eq!(link.mode & S_IFMT, S_IFLNK);
        assert!(fs.lstat("/missing").unwrap().is_none());
        assert!(matches!(
            fs.read_file("/via/hello", 64),
            Err(Ext4Error::Unsupported("symlink in path"))
        ));
    }

    #[test]
    fn fast_symlink_is_not_parsed_as_extents() {
        let mut img = sample_ext4_image();
        let off = sample_inode_offset(4);
        // Extent magic at the start of i_block must stay literal target bytes.
        img[off + 0x28] = 0x0A;
        img[off + 0x29] = 0xF3;
        img[off + 0x2A] = b'x';
        img[off + 0x2B] = b'y';
        img[off + 0x04] = 4;
        let fs = open_sample(&img);
        let expected = String::from_utf8_lossy(&[0x0A, 0xF3, b'x', b'y']).into_owned();
        assert_eq!(
            fs.read_file("/short", 64).unwrap(),
            FileRead::Symlink(expected)
        );
    }

    #[test]
    fn uninitialized_extent_reads_as_zeros() {
        let mut img = sample_ext4_image();
        let len_off = sample_inode_offset(3) + 0x28 + 12 + 4;
        let len = u16::from_le_bytes([img[len_off], img[len_off + 1]]) | 0x8000;
        img[len_off..len_off + 2].copy_from_slice(&len.to_le_bytes());
        let fs = open_sample(&img);
        assert_eq!(
            fs.read_file("/hello", 64).unwrap(),
            FileRead::Regular(vec![0; 11])
        );
    }

    #[test]
    fn corrupt_images_fail_without_panicking() {
        let mut bad_magic = sample_ext4_image();
        bad_magic[1024 + 0x38] = 0;
        assert!(matches!(
            Ext4Ro::open(MemReader(&bad_magic), 0, bad_magic.len() as u64),
            Err(Ext4Error::Corrupt("bad superblock magic"))
        ));

        let mut deep = sample_ext4_image();
        let depth = sample_inode_offset(3) + 0x28 + 6;
        deep[depth] = 6;
        let fs = open_sample(&deep);
        assert!(matches!(
            fs.read_file("/hello", 64),
            Err(Ext4Error::Corrupt("extent depth"))
        ));

        let mut zero_rec = sample_ext4_image();
        zero_rec[5 * 4096 + 4] = 0;
        zero_rec[5 * 4096 + 5] = 0;
        let fs = open_sample(&zero_rec);
        assert!(matches!(
            fs.read_file("/hello", 64),
            Err(Ext4Error::Corrupt("directory entry length"))
        ));

        let mut far = sample_ext4_image();
        let start = sample_inode_offset(3) + 0x28 + 20;
        far[start..start + 4].copy_from_slice(&100u32.to_le_bytes());
        let fs = open_sample(&far);
        assert!(matches!(
            fs.read_file("/hello", 64),
            Err(Ext4Error::Corrupt("extent block out of range"))
        ));
    }

    #[test]
    fn file_over_the_cap_is_rejected() {
        let img = sample_ext4_image();
        let fs = open_sample(&img);
        assert!(matches!(
            fs.read_file("/hole", 100),
            Err(Ext4Error::TooLarge)
        ));
    }

    #[test]
    fn mkfs_fixtures_match_old_and_new_features() {
        let new = gunzip_fixture("ext4-new.img.gz");
        let fs = Ext4Ro::open(MemReader(&new), 0, new.len() as u64).unwrap();
        assert_eq!(
            fs.read_file("/etc/os-release", 4096).unwrap(),
            FileRead::Symlink("../usr/lib/os-release".into())
        );
        match fs.read_file("/usr/lib/os-release", 4096).unwrap() {
            FileRead::Regular(bytes) => assert!(bytes.windows(9).any(|w| w == b"ID=debian")),
            other => panic!("os-release: {other:?}"),
        }
        match fs.read_file("/etc/debian_version", 64).unwrap() {
            FileRead::Regular(bytes) => assert_eq!(bytes, b"13.7\n"),
            other => panic!("debian_version: {other:?}"),
        }
        assert_eq!(
            fs.read_file("/etc/machine-id", 64).unwrap(),
            FileRead::Regular(Vec::new())
        );
        assert_eq!(
            fs.read_file("/etc/short", 64).unwrap(),
            FileRead::Symlink("fast-target".into())
        );
        match fs.read_file("/etc/long", 128).unwrap() {
            FileRead::Symlink(target) => assert_eq!(target.len(), 70),
            other => panic!("long symlink: {other:?}"),
        }
        assert!(matches!(
            fs.read_file("/many/f000", 16).unwrap(),
            FileRead::Regular(_)
        ));
        assert!(matches!(
            fs.read_file("/many/f599", 16).unwrap(),
            FileRead::Regular(_)
        ));

        let old = gunzip_fixture("ext4-old.img.gz");
        let fs = Ext4Ro::open(MemReader(&old), 0, old.len() as u64).unwrap();
        match fs.read_file("/etc/debian_version", 64).unwrap() {
            FileRead::Regular(bytes) => assert_eq!(bytes, b"13.3\n"),
            other => panic!("old debian_version: {other:?}"),
        }
        match fs.read_file("/etc/os-release", 4096).unwrap() {
            FileRead::Regular(bytes) => assert!(bytes.windows(9).any(|w| w == b"ID=debian")),
            other => panic!("old os-release: {other:?}"),
        }
        match fs.read_file("/etc/long", 128).unwrap() {
            FileRead::Symlink(target) => assert_eq!(target.len(), 70),
            other => panic!("old long symlink: {other:?}"),
        }
    }

    #[test]
    #[ignore = "set VIBE_TEST_DEBIAN_NEW_RAW and VIBE_TEST_DEBIAN_OLD_RAW"]
    fn real_debian_root_matches_recorded_files() {
        let Some(new) = std::env::var("VIBE_TEST_DEBIAN_NEW_RAW").ok() else {
            eprintln!("skipping real ext4 fixtures; set VIBE_TEST_DEBIAN_NEW_RAW");
            return;
        };
        let Some(old) = std::env::var("VIBE_TEST_DEBIAN_OLD_RAW").ok() else {
            eprintln!("skipping real ext4 fixtures; set VIBE_TEST_DEBIAN_OLD_RAW");
            return;
        };
        assert_real_root(&new, b"13.7\n", b"uninitialized\n", false);
        assert_real_root(&old, b"13.3\n", b"", true);
    }

    fn assert_real_root(path: &str, version: &[u8], machine_id: &[u8], password_empty: bool) {
        use crate::gpt::{read_gpt, select_layout};
        use std::fs::File;
        let file = File::open(path).unwrap();
        let len = file.metadata().unwrap().len();
        let mut cursor = File::open(path).unwrap();
        let gpt = read_gpt(&mut cursor).unwrap();
        let layout = select_layout(&gpt, len).unwrap();
        let offset = layout.root.first_lba * 512;
        let part_len = (layout.root.last_lba - layout.root.first_lba + 1) * 512;
        let fs = Ext4Ro::open(&file, offset, part_len).unwrap();
        assert!(matches!(
            fs.read_file("/etc/os-release", 4096).unwrap(),
            FileRead::Symlink(target) if target == "../usr/lib/os-release"
        ));
        match fs.read_file("/etc/debian_version", 64).unwrap() {
            FileRead::Regular(bytes) => assert_eq!(bytes, version, "{path}"),
            other => panic!("{path} debian_version: {other:?}"),
        }
        match fs.read_file("/etc/machine-id", 64).unwrap() {
            FileRead::Regular(bytes) => assert_eq!(bytes, machine_id, "{path}"),
            other => panic!("{path} machine-id: {other:?}"),
        }
        let mut shadow = match fs.read_file("/etc/shadow", 1024 * 1024).unwrap() {
            FileRead::Regular(bytes) => bytes,
            other => panic!("{path} shadow: {other:?}"),
        };
        let empty = shadow
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"root:") && line.get(5).copied() == Some(b':'));
        shadow.fill(0);
        assert_eq!(empty, password_empty, "{path}");
    }

    #[test]
    fn fuzz_rewrites_do_not_panic_or_hang() {
        let base = gunzip_fixture("ext4-new.img.gz");
        let mut state = 0x1234_5678_9abc_def0u64;
        for _ in 0..24 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut img = base.clone();
            for step in 0..4 {
                let idx = (state.wrapping_add(step * 17) as usize) % img.len();
                img[idx] ^= (state >> 8) as u8;
            }
            let Ok(fs) = Ext4Ro::open(MemReader(&img), 0, img.len() as u64) else {
                continue;
            };
            let _ = fs.read_file("/etc/os-release", 64 * 1024);
            let _ = fs.read_file("/usr/lib/os-release", 64 * 1024);
            let _ = fs.read_file("/etc/shadow", 1024 * 1024);
            let _ = fs.read_file("/many/f010", 64);
        }
    }
}
