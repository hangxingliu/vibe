//! Linear directory lookup.

use super::{
    DIR_SCAN_LIMIT, EXTENTS_FL, Ext4Error, Ext4Ro, ReadAt, div_ceil, inode::Inode, u16_at, u32_at,
};

impl<R: ReadAt> Ext4Ro<R> {
    pub(super) fn lookup(&self, dir: &Inode, name: &[u8]) -> Result<Option<u32>, Ext4Error> {
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

    /// Find `name` in one linear directory block.
    ///
    /// Each entry is `inode` (u32, 0 for an unused slot), `rec_len` (u16, bytes
    /// to the next entry), the name length and then the name bytes from offset
    /// 8. Without the `filetype` feature the name length is a u16 at offset 6.
    /// With it, `name_len` is only the byte at offset 6 and offset 7 holds the
    /// file type.
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
}
