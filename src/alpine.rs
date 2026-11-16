//! Pinned Alpine maintenance image.
//!
//! The cached raw is a read-only input. Every maintenance run clones it.
//! A valid raw plus a valid archive does not touch the network. A corrupt
//! archive is deleted and downloaded again even when the raw is still good.
//!
//! The preferred image directory is the managed copy: it is locked, repaired,
//! and downloaded into. A raw in any other image directory is only reused when
//! its partition UUID checks out; it is never locked, deleted, or re-extracted.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{
    cache::FileLock,
    gpt::{self, FsKind},
    image_dirs::ImageDirs,
    images::{self, FetchError},
};

pub(crate) const ALPINE_IMAGE_NAME: &str = "alpine-3.24.2-aarch64-tiny-r1";
pub(crate) const ALPINE_ARCHIVE_URL: &str = "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/cloud/alpine-3.24.2-aarch64-tiny-r1.raw.tar.gz";
pub(crate) const ALPINE_ARCHIVE_SHA512: &str = "c66badf3648147e0e06f710d600698ec35804af70b00667742b4dce9f28df5cb516190ea90d63792c3cf970a6541aa675ffe1ddddaed13df1abde1fe6f43698b";
/// ext4 UUID of partition 2. Alpine finds its own root by the label `/`.
pub(crate) const ALPINE_ROOT_FS_UUID: &str = "d51a6213-c765-4b85-9105-1b438c523993";
pub(crate) const ALPINE_ROOT_PART: u32 = 2;

/// Errors of [`ensure_alpine_image`]. Every message starts with `[download] `.
/// `Download` and `Checksum` name the URL, the expected SHA-512, and the
/// path where the user can place the archive by hand.
#[derive(Debug)]
pub(crate) enum AlpineError {
    Download {
        url: String,
        sha: String,
        path: PathBuf,
        detail: String,
    },
    Checksum {
        url: String,
        sha: String,
        path: PathBuf,
        detail: String,
    },
    Extract {
        detail: String,
    },
    Verify {
        detail: String,
    },
    Prepare {
        detail: String,
    },
}

impl std::fmt::Display for AlpineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlpineError::Download {
                url,
                sha,
                path,
                detail,
            } => write!(
                f,
                "[download] failed to download {url}: {detail}. Expected SHA-512 {sha}. Place the archive manually at {}.",
                path.display()
            ),
            AlpineError::Checksum {
                url,
                sha,
                path,
                detail,
            } => write!(
                f,
                "[download] checksum: {detail}. Expected SHA-512 {sha} for {url}. Place the archive manually at {}.",
                path.display()
            ),
            AlpineError::Extract { detail } => write!(f, "[download] extract: {detail}"),
            AlpineError::Verify { detail } => write!(f, "[download] verify: {detail}"),
            AlpineError::Prepare { detail } => write!(f, "[download] cache: {detail}"),
        }
    }
}

impl std::error::Error for AlpineError {}

impl From<io::Error> for AlpineError {
    fn from(err: io::Error) -> Self {
        AlpineError::Prepare {
            detail: err.to_string(),
        }
    }
}

pub(crate) fn ensure_alpine_image(dirs: &ImageDirs) -> Result<PathBuf, AlpineError> {
    let others: Vec<&Path> = dirs
        .existing()
        .filter(|dir| *dir != dirs.preferred())
        .collect();
    ensure_alpine_image_in(
        dirs.preferred(),
        &others,
        ALPINE_ARCHIVE_URL,
        ALPINE_ARCHIVE_SHA512,
        ALPINE_IMAGE_NAME,
    )
}

/// A raw in `preferred` goes through [`ensure_alpine_image_at`]. Without one,
/// the first valid raw in `others` is used as-is. Otherwise `preferred` is
/// filled by [`ensure_alpine_image_at`].
fn ensure_alpine_image_in(
    preferred: &Path,
    others: &[&Path],
    url: &str,
    sha512: &str,
    name: &str,
) -> Result<PathBuf, AlpineError> {
    let file_name = format!("{name}.raw");
    if !preferred.join(&file_name).exists() {
        for dir in others {
            let raw = dir.join(&file_name);
            if !raw.is_file() {
                continue;
            }
            match alpine_root_uuid_matches(&raw) {
                Ok(true) => return Ok(raw),
                Ok(false) => eprintln!(
                    "Warning: skipping the Alpine maintenance image {}: partition {ALPINE_ROOT_PART} ext4 UUID is not {ALPINE_ROOT_FS_UUID}. The file was not modified.",
                    raw.display()
                ),
                Err(err) => eprintln!(
                    "Warning: skipping the Alpine maintenance image {}: {err}. The file was not modified.",
                    raw.display()
                ),
            }
        }
    }
    ensure_alpine_image_at(preferred, url, sha512, name)
}

/// `name` is the cache stem: `{name}.raw` and `{name}.raw.tar.gz`.
pub(crate) fn ensure_alpine_image_at(
    cache_dir: &Path,
    url: &str,
    sha512: &str,
    name: &str,
) -> Result<PathBuf, AlpineError> {
    fs::create_dir_all(cache_dir)?;
    let _lock =
        FileLock::acquire(&cache_dir.join("alpine.lock")).map_err(|err| AlpineError::Prepare {
            detail: format!("cannot lock alpine.lock: {err}"),
        })?;
    let archive = cache_dir.join(format!("{name}.raw.tar.gz"));
    let raw = cache_dir.join(format!("{name}.raw"));
    ensure_locked(&archive, &raw, url, sha512)?;
    Ok(raw)
}

fn ensure_locked(archive: &Path, raw: &Path, url: &str, sha512: &str) -> Result<(), AlpineError> {
    let raw_ok = raw.exists() && alpine_root_uuid_matches(raw)?;
    if archive.exists() && images::verify_sha512(archive, sha512).is_err() {
        fs::remove_file(archive)?;
    }
    if !archive.exists() {
        fetch_archive(url, sha512, archive)?;
    }
    if raw.exists() && !raw_ok {
        fs::remove_file(raw)?;
    }
    if !raw.exists() {
        images::extract_disk_raw(archive, raw).map_err(|err| AlpineError::Extract {
            detail: err.to_string(),
        })?;
        if !alpine_root_uuid_matches(raw)? {
            let _ = fs::remove_file(raw);
            return Err(AlpineError::Verify {
                detail: format!(
                    "extracted raw partition {ALPINE_ROOT_PART} UUID is not {ALPINE_ROOT_FS_UUID}"
                ),
            });
        }
    }
    Ok(())
}

/// Download `archive` through [`images::fetch_verified`]. A failure leaves
/// neither `archive` nor its `.part` file behind.
fn fetch_archive(url: &str, sha512: &str, archive: &Path) -> Result<(), AlpineError> {
    match images::fetch_verified(url, sha512, archive) {
        Ok(()) => Ok(()),
        Err(FetchError {
            stage: "checksum",
            message,
        }) => Err(AlpineError::Checksum {
            url: url.to_string(),
            sha: sha512.to_string(),
            path: archive.to_path_buf(),
            detail: message,
        }),
        Err(FetchError { message, .. }) => Err(AlpineError::Download {
            url: url.to_string(),
            sha: sha512.to_string(),
            path: archive.to_path_buf(),
            detail: message,
        }),
    }
}

fn alpine_root_uuid_matches(raw: &Path) -> Result<bool, AlpineError> {
    let mut file = fs::File::open(raw)?;
    let gpt = match gpt::read_gpt(&mut file) {
        Ok(gpt) => gpt,
        Err(_) => return Ok(false),
    };
    let labels = gpt::scan_labels(&mut file, &gpt).map_err(|err| AlpineError::Verify {
        detail: err.to_string(),
    })?;
    Ok(labels.iter().any(|label| {
        label.part == Some(ALPINE_ROOT_PART)
            && label.kind == FsKind::Ext4
            && label.uuid.eq_ignore_ascii_case(ALPINE_ROOT_FS_UUID)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpt::{LINUX_FILESYSTEM_GUID, TestPart, build_gpt};
    use std::process::Command;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vibe-alpine-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn synthetic_raw() -> Vec<u8> {
        let parts = vec![TestPart {
            index: ALPINE_ROOT_PART,
            type_guid: LINUX_FILESYSTEM_GUID,
            unique_guid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            first_lba: 2048,
            last_lba: 4095,
            name: "alpine",
        }];
        let disk_bytes = 4096 * 512;
        let mut image = build_gpt(&parts, disk_bytes, 128);
        image.resize(disk_bytes as usize, 0);
        let sb = 2048 * 512 + 1024;
        image[sb + 0x38] = 0x53;
        image[sb + 0x39] = 0xEF;
        let uuid = [
            0xd5, 0x1a, 0x62, 0x13, 0xc7, 0x65, 0x4b, 0x85, 0x91, 0x05, 0x1b, 0x43, 0x8c, 0x52,
            0x39, 0x93,
        ];
        image[sb + 0x68..sb + 0x78].copy_from_slice(&uuid);
        image[sb + 0x78] = b'/';
        image
    }

    fn write_archive(dir: &Path, raw_bytes: &[u8]) -> (PathBuf, String) {
        let staging = dir.join("staging");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("disk.raw"), raw_bytes).unwrap();
        let archive = dir.join("good.raw.tar.gz");
        let status = Command::new("tar")
            .args([
                "-czf",
                &archive.to_string_lossy(),
                "-C",
                &staging.to_string_lossy(),
                "disk.raw",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let sha = sha512_hex(&archive);
        (archive, sha)
    }

    fn sha512_hex(path: &Path) -> String {
        let output = Command::new("/usr/bin/shasum")
            .args(["--algorithm", "512"])
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    }

    #[test]
    fn cache_hit_does_not_download() {
        let dir = temp_dir("hit");
        let raw_bytes = synthetic_raw();
        let (archive, sha) = write_archive(&dir, &raw_bytes);
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).unwrap();
        let name = "alpine-test";
        fs::copy(&archive, cache.join(format!("{name}.raw.tar.gz"))).unwrap();
        fs::write(cache.join(format!("{name}.raw")), &raw_bytes).unwrap();
        let got = ensure_alpine_image_at(&cache, "http://127.0.0.1:9/nope", &sha, name).unwrap();
        assert_eq!(fs::read(&got).unwrap(), raw_bytes);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_archive_is_replaced_from_the_url() {
        let dir = temp_dir("corrupt");
        let raw_bytes = synthetic_raw();
        let (archive, sha) = write_archive(&dir, &raw_bytes);
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).unwrap();
        let name = "alpine-test";
        fs::write(cache.join(format!("{name}.raw.tar.gz")), b"not-a-tar").unwrap();
        fs::write(cache.join(format!("{name}.raw")), &raw_bytes).unwrap();
        let url = format!("file://{}", archive.display());
        ensure_alpine_image_at(&cache, &url, &sha, name).unwrap();
        assert!(images::verify_sha512(&cache.join(format!("{name}.raw.tar.gz")), &sha).is_ok());
        assert_eq!(
            fs::read(cache.join(format!("{name}.raw"))).unwrap(),
            raw_bytes
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_raw_is_extracted_without_a_download() {
        let dir = temp_dir("extract");
        let raw_bytes = synthetic_raw();
        let (archive, sha) = write_archive(&dir, &raw_bytes);
        let cache = dir.join("cache");
        fs::create_dir_all(&cache).unwrap();
        let name = "alpine-test";
        fs::copy(&archive, cache.join(format!("{name}.raw.tar.gz"))).unwrap();
        let got = ensure_alpine_image_at(&cache, "http://127.0.0.1:9/nope", &sha, name).unwrap();
        assert_eq!(fs::read(&got).unwrap(), raw_bytes);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_raw_in_another_directory_is_reused_without_touching_preferred() {
        let dir = temp_dir("other-hit");
        let raw_bytes = synthetic_raw();
        let preferred = dir.join("preferred");
        let other = dir.join("other");
        fs::create_dir_all(&preferred).unwrap();
        fs::create_dir_all(&other).unwrap();
        let name = "alpine-test";
        fs::write(other.join(format!("{name}.raw")), &raw_bytes).unwrap();
        let got = ensure_alpine_image_in(
            &preferred,
            &[other.as_path()],
            "http://127.0.0.1:9/nope",
            "abc",
            name,
        )
        .unwrap();
        assert_eq!(got, other.join(format!("{name}.raw")));
        assert_eq!(fs::read_dir(&preferred).unwrap().count(), 0);
        assert!(!other.join("alpine.lock").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_raw_in_another_directory_is_kept_and_skipped() {
        let dir = temp_dir("other-bad");
        let raw_bytes = synthetic_raw();
        let (archive, sha) = write_archive(&dir, &raw_bytes);
        let preferred = dir.join("preferred");
        let other = dir.join("other");
        fs::create_dir_all(&preferred).unwrap();
        fs::create_dir_all(&other).unwrap();
        let name = "alpine-test";
        let bad = other.join(format!("{name}.raw"));
        fs::write(&bad, b"not-an-alpine-disk").unwrap();
        fs::copy(&archive, preferred.join(format!("{name}.raw.tar.gz"))).unwrap();
        let got = ensure_alpine_image_in(
            &preferred,
            &[other.as_path()],
            "http://127.0.0.1:9/nope",
            &sha,
            name,
        )
        .unwrap();
        assert_eq!(got, preferred.join(format!("{name}.raw")));
        assert_eq!(fs::read(&got).unwrap(), raw_bytes);
        assert_eq!(fs::read(&bad).unwrap(), b"not-an-alpine-disk");
        let _ = fs::remove_dir_all(&dir);
    }
}
