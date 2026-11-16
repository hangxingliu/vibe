//! In-memory ext4 images and gzip fixtures for tests.

use std::io::{self, ErrorKind};

use super::{
    EXT4_MAGIC, EXTENT_MAGIC, EXTENTS_FL, Ext4Ro, INCOMPAT_EXTENTS, INCOMPAT_FILETYPE, ReadAt,
    S_IFDIR, S_IFLNK, S_IFREG,
};

pub(crate) struct MemReader<'a>(pub &'a [u8]);

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

struct DirEnt {
    ino: u32,
    name: &'static str,
    ty: u8,
}

pub(super) fn sample_inode_offset(ino: u32) -> usize {
    4 * 4096 + (ino as usize - 1) * 256
}

fn write_extent_inode(img: &mut [u8], ino: u32, mode: u16, size: u64, extents: &[(u32, u16, u64)]) {
    let off = sample_inode_offset(ino);
    put_u16(img, off, mode);
    put_u32(img, off + 0x04, size as u32);
    put_u32(img, off + 0x6C, (size >> 32) as u32);
    put_u32(img, off + 0x20, EXTENTS_FL);
    write_extent_header(&mut img[off + 0x28..off + 0x28 + 60], extents);
}

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

fn write_fast_symlink(img: &mut [u8], ino: u32, target: &[u8]) {
    assert!(target.len() < 60);
    let off = sample_inode_offset(ino);
    put_u16(img, off, S_IFLNK | 0o777);
    put_u32(img, off + 0x04, target.len() as u32);
    img[off + 0x28..off + 0x28 + target.len()].copy_from_slice(target);
}

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

pub(super) fn put_u16(buf: &mut [u8], off: usize, value: u16) {
    buf[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, value: u32) {
    buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn open_sample(img: &[u8]) -> Ext4Ro<MemReader<'_>> {
    Ext4Ro::open(MemReader(img), 0, img.len() as u64).unwrap()
}

/// Gzip images written by `tests/fixtures/make-ext4.sh`.
/// Keep this list aligned with the names that script publishes.
const EXT4_FIXTURE_NAMES: &[&str] = &[
    "ext4-new.img.gz",
    "ext4-old.img.gz",
    "ext4-mid-nonempty.img.gz",
    "ext4-mid-symlink.img.gz",
    "ext4-shadow-locked.img.gz",
    "ext4-shadow-two.img.gz",
    "ext4-shadow-none.img.gz",
    "ext4-dbus-outside.img.gz",
];

fn ext4_fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Fixture names that are not regular files in `dir`.
fn missing_ext4_fixtures(dir: &std::path::Path) -> Vec<&'static str> {
    EXT4_FIXTURE_NAMES
        .iter()
        .copied()
        .filter(|name| !dir.join(name).is_file())
        .collect()
}

/// Run `run_script` when any required gzip image is absent.
/// A present regular file is left as it is.
fn prepare_ext4_fixtures(
    dir: &std::path::Path,
    run_script: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if missing_ext4_fixtures(dir).is_empty() {
        return Ok(());
    }
    run_script()?;
    let missing = missing_ext4_fixtures(dir);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "tests/fixtures/make-ext4.sh did not create {}",
            missing.join(", ")
        ))
    }
}

fn run_make_ext4(script: &std::path::Path) -> Result<(), String> {
    let output = std::process::Command::new("/bin/sh")
        .arg(script)
        .output()
        .map_err(|err| format!("cannot run {}: {err}", script.display()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{} exited {}\n{}{}",
        script.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// Build the gitignored ext4 fixtures once per process when any one is missing.
fn ensure_ext4_fixtures() {
    use std::sync::OnceLock;
    static READY: OnceLock<Result<(), String>> = OnceLock::new();
    let result = READY.get_or_init(|| {
        let dir = ext4_fixture_dir();
        let script = dir.join("make-ext4.sh");
        prepare_ext4_fixtures(&dir, || {
            eprintln!("ext4 fixtures missing; running {}", script.display());
            run_make_ext4(&script)
        })
    });
    if let Err(err) = result {
        panic!("ext4 fixtures: {err}");
    }
}

/// Gunzip one image from `tests/fixtures`, building the set first when needed.
pub(crate) fn gunzip_ext4_fixture(name: &str) -> Vec<u8> {
    ensure_ext4_fixtures();
    let path = ext4_fixture_dir().join(name);
    let output = std::process::Command::new("/usr/bin/gzip")
        .arg("-dc")
        .arg(&path)
        .output()
        .unwrap_or_else(|err| panic!("gzip {name}: {err}"));
    assert!(
        output.status.success(),
        "gzip {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
