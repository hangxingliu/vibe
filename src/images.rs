//! Image files in the image directories (see [`ImageDirs`]): the downloaded
//! Debian base image, named images, and per-project instance disks.
//!
//! Alpine downloads use [`fetch_verified`] (a `.part` file, then rename).
//! The Debian path keeps its original messages and does not re-download an
//! archive that is already large enough.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{cache, image_dirs::ImageDirs};

pub(crate) const DEBIAN_COMPRESSED_DISK_URL: &str = "https://cdimage.debian.org/cdimage/cloud/trixie/20261001-2618/debian-13-nocloud-arm64-20261001-2618.tar.xz";
const DEBIAN_COMPRESSED_SHA: &str = "c049582fe2d13d9e0c1a4767ade3a32bed217774bd6e39ac8fc60ca78534bfc258a8c52363ddfad639d5db7eacc2550688d5393f16aaf62bc5d19ecd4a7e6170";
const DEBIAN_COMPRESSED_FILE_NAME: &str = "disk.raw";
const DEBIAN_COMPRESSED_SIZE_BYTES: u64 = 280000000; // 280901576, 284558188

pub(crate) fn assert_valid_image_name(name: &str) {
    assert!(
        !name.is_empty()
            && name
                .chars()
                .all(|c| c == '-' || c == '_' || c.is_ascii_alphanumeric()),
        "Image name must be alphanumeric, dash, or underscore."
    );
}

pub(crate) fn image_path(dir: &Path, name: &str) -> PathBuf {
    assert_valid_image_name(name);
    dir.join(format!("{name}.raw"))
}

/// `<name>.raw` in the first image directory that has it.
pub(crate) fn find_named_image(dirs: &ImageDirs, name: &str) -> Option<PathBuf> {
    assert_valid_image_name(name);
    dirs.find_file(&format!("{name}.raw"))
}

/// Where a new image called `name` is created: the preferred directory.
pub(crate) fn new_image_path(dirs: &ImageDirs, name: &str) -> PathBuf {
    image_path(dirs.preferred(), name)
}

/// The Debian base image files to use.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DebianBase {
    pub raw: PathBuf,
    pub compressed: PathBuf,
    /// `raw` already exists in one of the image directories.
    pub found: bool,
}

/// An existing Debian `.raw` in search order, used read-only. Otherwise the
/// `.raw` and `.tar.xz` paths in the preferred directory, where
/// [`ensure_base_image`] downloads and extracts. An archive in another
/// directory is not reused.
pub(crate) fn locate_debian_base(dirs: &ImageDirs) -> DebianBase {
    let compressed_name = DEBIAN_COMPRESSED_DISK_URL
        .rsplit('/')
        .next()
        .expect("URL has a file name");
    let raw_name = format!("{}.raw", compressed_name.trim_end_matches(".tar.xz"));
    let compressed = dirs.preferred().join(compressed_name);
    match dirs.find_file(&raw_name) {
        Some(raw) => DebianBase {
            raw,
            compressed,
            found: true,
        },
        None => DebianBase {
            raw: dirs.preferred().join(raw_name),
            compressed,
            found: false,
        },
    }
}

/// An archive needs a download when it is missing or smaller than the known
/// Debian archive. A large file with a bad checksum is left in place.
fn archive_needs_download(exists: bool, len: Option<u64>) -> bool {
    !exists || len.unwrap_or(0) < DEBIAN_COMPRESSED_SIZE_BYTES
}

pub(crate) fn ensure_base_image(
    base_raw: &Path,
    base_compressed: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if base_raw.exists() {
        return Ok(());
    }

    let compressed_len = std::fs::metadata(base_compressed)
        .ok()
        .map(|meta| meta.len());
    if archive_needs_download(base_compressed.exists(), compressed_len) {
        println!("Downloading base image...");
        let proxy_args = download_proxy_args().map_err(|err| format!("[download] {err}"))?;
        let status = Command::new("curl")
            .args(&proxy_args)
            .args([
                "--continue-at",
                "-",
                "--compressed",
                "--location",
                "--fail",
                "-o",
                &base_compressed.to_string_lossy(),
                DEBIAN_COMPRESSED_DISK_URL,
            ])
            .status()
            .map_err(|err| format!("[download] failed to run curl: {err}"))?;
        if !status.success() {
            return Err(format!(
                "[download] Failed to download base image from {DEBIAN_COMPRESSED_DISK_URL}"
            )
            .into());
        }
    }

    if verify_sha512(base_compressed, DEBIAN_COMPRESSED_SHA).is_err() {
        return Err(
            format!("[download] SHA validation failed for {DEBIAN_COMPRESSED_DISK_URL}").into(),
        );
    }

    println!("Decompressing base image...");
    if extract_disk_raw(base_compressed, base_raw).is_err() {
        return Err(format!(
            "[download] Failed to decompress base image {}",
            base_compressed.display()
        )
        .into());
    }

    Ok(())
}

/// The curl proxy arguments for a download, after printing which proxy is
/// used. curl always gets `--proxy`, so it never picks a proxy on its own.
fn download_proxy_args() -> Result<Vec<String>, String> {
    let proxy = crate::proxy::download_proxy()?;
    for line in proxy.describe().lines() {
        if line.starts_with("warning: ") {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    Ok(proxy.curl_args())
}

/// Why [`fetch_verified`] failed. `stage` is `download` or `checksum`.
#[derive(Debug)]
pub(crate) struct FetchError {
    pub stage: &'static str,
    pub message: String,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.stage, self.message)
    }
}

impl std::error::Error for FetchError {}

/// Download `url` to `{archive_dest}.part`, check SHA-512, then rename.
/// A failed download or checksum deletes the partial file and does not leave
/// `archive_dest`.
pub(crate) fn fetch_verified(
    url: &str,
    sha512: &str,
    archive_dest: &Path,
) -> Result<(), FetchError> {
    if let Some(parent) = archive_dest.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|err| FetchError {
            stage: "download",
            message: format!("cannot create {}: {err}", parent.display()),
        })?;
    }
    let part = part_path(archive_dest);
    let proxy_args = download_proxy_args().map_err(|message| FetchError {
        stage: "download",
        message,
    })?;
    let status = Command::new("curl")
        .args(&proxy_args)
        .args([
            "--continue-at",
            "-",
            "--compressed",
            "--location",
            "--fail",
            "-o",
        ])
        .arg(&part)
        .arg(url)
        .status()
        .map_err(|err| {
            let _ = fs::remove_file(&part);
            FetchError {
                stage: "download",
                message: format!("failed to run curl: {err}"),
            }
        })?;
    if !status.success() {
        let _ = fs::remove_file(&part);
        return Err(FetchError {
            stage: "download",
            message: format!("curl exited {status}"),
        });
    }
    if let Err(err) = verify_sha512(&part, sha512) {
        let _ = fs::remove_file(&part);
        return Err(FetchError {
            stage: "checksum",
            message: err,
        });
    }
    fs::rename(&part, archive_dest).map_err(|err| {
        let _ = fs::remove_file(&part);
        FetchError {
            stage: "download",
            message: format!("cannot publish {}: {err}", archive_dest.display()),
        }
    })?;
    Ok(())
}

pub(crate) fn part_path(archive_dest: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", archive_dest.display()))
}

/// Check `path` against a SHA-512 hex digest using `/usr/bin/shasum`.
pub(crate) fn verify_sha512(path: &Path, expected: &str) -> Result<(), String> {
    let input = format!("{expected}  {}\n", path.display());
    let mut child = Command::new("/usr/bin/shasum")
        .args(["--algorithm", "512", "--check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("failed to spawn shasum: {err}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "failed to open shasum stdin".to_string())?
        .write_all(input.as_bytes())
        .map_err(|err| format!("failed to write shasum input: {err}"))?;
    let status = child
        .wait()
        .map_err(|err| format!("failed to wait on shasum: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("SHA-512 does not match for {}", path.display()))
    }
}

/// Extract the `disk.raw` member of `archive` and publish it as `raw_dest`.
/// The bytes land in a temporary file in the same directory and are renamed
/// only after `tar` succeeds.
pub(crate) fn extract_disk_raw(
    archive: &Path,
    raw_dest: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = raw_dest.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let (file, temp) = cache::exclusive_temp_next_to(raw_dest)?;
    let extracted = Command::new("tar")
        .args([
            "-xOf",
            &archive.to_string_lossy(),
            DEBIAN_COMPRESSED_FILE_NAME,
        ])
        .stdout(Stdio::from(file))
        .status();
    match extracted {
        Ok(status) if status.success() => {
            cache::replace(&temp, raw_dest)?;
            Ok(())
        }
        Ok(status) => {
            let _ = fs::remove_file(&temp);
            Err(format!("tar exited {status} extracting {}", archive.display()).into())
        }
        Err(err) => {
            let _ = fs::remove_file(&temp);
            Err(err.into())
        }
    }
}

pub(crate) fn ensure_instance_disk(
    instance_raw: &Path,
    template_raw: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if instance_raw.exists() {
        println!(
            "The instance disk {} has been copied already",
            instance_raw.display()
        );
        return Ok(());
    }

    println!("Creating instance disk from {}...", template_raw.display());
    std::fs::create_dir_all(instance_raw.parent().unwrap())?;
    fs::copy(template_raw, instance_raw)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vibe-images-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    #[should_panic(expected = "Image name must be alphanumeric")]
    fn find_named_image_rejects_invalid_names() {
        find_named_image(&ImageDirs::single(PathBuf::from("/nonexistent")), "../x");
    }

    #[test]
    fn debian_base_is_reused_from_any_directory_or_placed_in_the_preferred_one() {
        let root = temp_dir("debian-locate");
        let default = root.join("default");
        let pref = root.join("pref");
        let other = root.join("other");
        for dir in [&default, &pref, &other] {
            fs::create_dir_all(dir).unwrap();
        }
        let extra = format!("{}:{}", other.display(), pref.display());
        let dirs = ImageDirs::from_values(
            default.clone(),
            Some(std::ffi::OsStr::new(&extra)),
            Some(pref.as_os_str()),
        )
        .unwrap();

        let fresh = locate_debian_base(&dirs);
        assert!(!fresh.found);
        assert_eq!(fresh.raw.parent(), Some(pref.as_path()));
        assert_eq!(fresh.compressed.parent(), Some(pref.as_path()));
        assert!(fresh.raw.to_string_lossy().ends_with(".raw"));
        assert!(fresh.compressed.to_string_lossy().ends_with(".tar.xz"));

        let raw_name = fresh.raw.file_name().unwrap();
        fs::write(other.join(raw_name), b"").unwrap();
        let reused = locate_debian_base(&dirs);
        assert!(reused.found);
        assert_eq!(reused.raw, other.join(raw_name));
        assert_eq!(reused.compressed.parent(), Some(pref.as_path()));

        fs::write(default.join(raw_name), b"").unwrap();
        assert_eq!(locate_debian_base(&dirs).raw, default.join(raw_name));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn large_archive_does_not_need_another_download() {
        assert!(archive_needs_download(false, None));
        assert!(archive_needs_download(true, Some(0)));
        assert!(archive_needs_download(
            true,
            Some(DEBIAN_COMPRESSED_SIZE_BYTES - 1)
        ));
        assert!(!archive_needs_download(
            true,
            Some(DEBIAN_COMPRESSED_SIZE_BYTES)
        ));
    }

    #[test]
    fn large_bad_debian_archive_is_not_replaced() {
        let dir = temp_dir("debian-sha");
        let archive = dir.join("debian.tar.xz");
        let raw = dir.join("debian.raw");
        let file = fs::File::create(&archive).unwrap();
        file.set_len(DEBIAN_COMPRESSED_SIZE_BYTES).unwrap();
        drop(file);
        let before = fs::metadata(&archive).unwrap().modified().unwrap();
        let err = ensure_base_image(&raw, &archive).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("[download] SHA validation failed for https://cdimage.debian.org/"),
            "{err}"
        );
        assert!(!raw.exists());
        assert_eq!(
            fs::metadata(&archive).unwrap().len(),
            DEBIAN_COMPRESSED_SIZE_BYTES
        );
        assert_eq!(fs::metadata(&archive).unwrap().modified().unwrap(), before);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_verified_publishes_a_matching_file_url() {
        let dir = temp_dir("fetch-ok");
        let source = dir.join("payload.bin");
        fs::write(&source, b"alpine-archive-bytes").unwrap();
        let sha = sha512_hex(&source);
        let dest = dir.join("out.bin");
        let url = format!("file://{}", source.display());
        fetch_verified(&url, &sha, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"alpine-archive-bytes");
        assert!(!part_path(&dest).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_verified_bad_sha_removes_the_partial_file() {
        let dir = temp_dir("fetch-bad");
        let source = dir.join("payload.bin");
        fs::write(&source, b"not-the-expected-bytes").unwrap();
        let dest = dir.join("out.bin");
        let url = format!("file://{}", source.display());
        let err = fetch_verified(&url, &"ab".repeat(64), &dest).unwrap_err();
        assert_eq!(err.stage, "checksum");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        let _ = fs::remove_dir_all(&dir);
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
}
