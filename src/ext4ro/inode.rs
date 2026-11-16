//! Inode table lookup.

use super::{Ext4Error, Ext4Ro, ReadAt, u16_at, u32_at};

pub(super) struct Inode {
    pub(super) mode: u16,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) flags: u32,
    /// Size used for regular files and directories (includes `i_size_high`).
    pub(super) size: u64,
    /// Low 32 bits, which is the whole size of a symlink.
    pub(super) size_lo: u32,
    pub(super) iblock: [u8; 60],
}

impl<R: ReadAt> Ext4Ro<R> {
    /// Read inode `ino`.
    ///
    /// Inode numbers start at 1, so inode `ino` is slot `(ino - 1) %
    /// inodes_per_group` in the inode table of group `(ino - 1) /
    /// inodes_per_group`. The 60 bytes of `i_block` at 0x28 hold the extent
    /// tree root, or the target of a fast symlink.
    pub(super) fn read_inode(&self, ino: u32) -> Result<Inode, Ext4Error> {
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
}
