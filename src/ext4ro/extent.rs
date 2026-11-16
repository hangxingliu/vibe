//! Extent tree walk and extent-mapped reads.

use super::{
    EXTENT_MAGIC, EXTENTS_FL, Ext4Error, Ext4Ro, MAX_EXTENT_DEPTH, MAX_EXTENT_NODES, MAX_EXTENTS,
    ReadAt, inode::Inode, u16_at, u32_at,
};

pub(super) struct Extent {
    logical: u32,
    len: u32,
    start: u64,
    uninit: bool,
}

impl<R: ReadAt> Ext4Ro<R> {
    /// Read the first `size` bytes of an extent-mapped inode.
    ///
    /// The output starts zero-filled. Logical blocks with no extent (holes)
    /// are never written by ext4 and read as zeros. Uninitialized extents are
    /// allocated but not yet written, so ext4 also defines them to read as
    /// zeros; their on-disk blocks may hold stale data and are skipped.
    pub(super) fn read_mapped(
        &self,
        inode: &Inode,
        size: u64,
        max_len: usize,
    ) -> Result<Vec<u8>, Ext4Error> {
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

    pub(super) fn load_extents(&self, iblock: &[u8; 60]) -> Result<Vec<Extent>, Ext4Error> {
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

    /// Collect the leaf extents below one extent tree node.
    ///
    /// Every node starts with a 12-byte header: `eh_magic` (u16, 0xF30A),
    /// `eh_entries` (u16), `eh_max` (u16), `eh_depth` (u16), and a 4-byte
    /// generation. `eh_entries` 12-byte entries follow. At depth 0 they are
    /// leaf extents: `ee_block` (u32 first logical block), `ee_len` (u16),
    /// `ee_start_hi` (u16) and `ee_start_lo` (u32 first physical block). At
    /// depth above 0 they are index entries: `ei_block` (u32), `ei_leaf_lo`
    /// (u32) and `ei_leaf_hi` (u16) naming the child node block, plus 2
    /// unused bytes.
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
                // The high bit of ee_len marks an uninitialized extent; the
                // low 15 bits are the length in blocks.
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
            // A child must sit exactly one level below its parent. This also
            // keeps a block that points back at an ancestor from recursing.
            let child_depth = u16_at(&block, 6)?;
            if child_depth != depth - 1 {
                return Err(Ext4Error::Corrupt("extent depth"));
            }
            self.walk_extent_node(&block, nodes, extents)?;
        }
        Ok(())
    }

    pub(super) fn phys_for(
        &self,
        extents: &[Extent],
        logical: u64,
    ) -> Result<Option<(u64, bool)>, Ext4Error> {
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
}
