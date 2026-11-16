use super::test_support::{MemReader, open_sample, put_u16, sample_inode_offset};
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
    let new = gunzip_ext4_fixture("ext4-new.img.gz");
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

    let old = gunzip_ext4_fixture("ext4-old.img.gz");
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
    let base = gunzip_ext4_fixture("ext4-new.img.gz");
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
