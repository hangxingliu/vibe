//! Cache locks, private maintenance directories, and atomic publish helpers.
//!
//! `flock` is per open file description. On this platform two descriptors in
//! one process still exclude each other, which is what the tests rely on.
//! A maintenance directory is `{prefix}{pid}-{nonce}` with mode 0700 so
//! `sweep_stale` can drop leftovers whose pid no longer exists.
//!
//! `FileLock::acquire` is the Alpine download lock. `try_acquire_existing`
//! locks `{disk}.lock` beside an existing disk, without changing the disk's
//! mode. The disk image itself is not locked: Virtualization.framework flocks
//! that file when it attaches the disk, and a lock already held by this
//! process makes the attachment invalid. `process_disk` takes that sibling
//! lock; `ensure_bootable` does not take a second one.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    os::fd::AsRawFd,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use crate::control::new_nonce;

#[derive(Debug)]
pub(crate) enum LockError {
    Io(io::Error),
    WouldBlock,
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::Io(err) => write!(f, "{err}"),
            LockError::WouldBlock => write!(f, "file is locked by another process"),
        }
    }
}

impl std::error::Error for LockError {}

impl From<io::Error> for LockError {
    fn from(err: io::Error) -> Self {
        if would_block(&err) {
            LockError::WouldBlock
        } else {
            LockError::Io(err)
        }
    }
}

pub(crate) struct FileLock {
    file: File,
}

impl FileLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, LockError> {
        let file = open_lock_file(path)?;
        flock_wait(file.as_raw_fd(), libc::LOCK_EX)?;
        Ok(Self { file })
    }

    /// Non-blocking lock used by tests. Provision does not call this yet.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn try_acquire(path: &Path) -> Result<Self, LockError> {
        let file = open_lock_file(path)?;
        flock_wait(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)?;
        Ok(Self { file })
    }

    /// Lock an existing disk for maintenance.
    ///
    /// The disk is opened read-write only long enough to prove this process
    /// can write it. The exclusive lock is `{disk}.lock` in the same directory.
    /// Holding `flock` on the disk image itself makes
    /// `VZDiskImageStorageDeviceAttachment` fail at start with
    /// "storage device attachment is invalid".
    pub(crate) fn try_acquire_existing(path: &Path) -> Result<Self, LockError> {
        {
            let writable = OpenOptions::new().read(true).write(true).open(path)?;
            drop(writable);
        }
        let file = open_lock_file(&disk_lock_path(path))?;
        flock_wait(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)?;
        Ok(Self { file })
    }
}

pub(crate) fn disk_lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("disk"))
        .to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn flock_wait(fd: i32, op: i32) -> Result<(), LockError> {
    loop {
        let rc = unsafe { libc::flock(fd, op) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() == ErrorKind::Interrupted {
            continue;
        }
        return Err(err.into());
    }
}

fn would_block(err: &io::Error) -> bool {
    err.kind() == ErrorKind::WouldBlock
        || err.raw_os_error() == Some(libc::EAGAIN)
        || err.raw_os_error() == Some(libc::EWOULDBLOCK)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloneKind {
    Cloned,
    Copied,
}

/// Clone `src` to `dest`. `clonefile` is tried first; a copy is the fallback.
/// `dest` must not already exist.
pub(crate) fn clone_file(src: &Path, dest: &Path) -> io::Result<CloneKind> {
    if dest.exists() {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "clone destination already exists",
        ));
    }
    let src_c = c_path(src)?;
    let dest_c = c_path(dest)?;
    let rc = unsafe { libc::clonefile(src_c.as_ptr(), dest_c.as_ptr(), 0) };
    if rc == 0 {
        return Ok(CloneKind::Cloned);
    }
    let err = io::Error::last_os_error();
    eprintln!(
        "clonefile failed ({err}); copying {} to {} instead",
        src.display(),
        dest.display()
    );
    fs::copy(src, dest)?;
    Ok(CloneKind::Copied)
}

/// Create `{name}.tmp.{pid}.{nonce}` in the same directory as `dest`.
pub(crate) fn exclusive_temp_next_to(dest: &Path) -> io::Result<(File, PathBuf)> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let name = dest
        .file_name()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "destination has no file name"))?;
    for _ in 0..8 {
        let nonce = new_nonce()?;
        let tmp_name = format!(
            "{}.tmp.{}.{}",
            name.to_string_lossy(),
            std::process::id(),
            nonce
        );
        let path = parent.join(tmp_name);
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => {
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
                return Ok((file, path));
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "could not create an exclusive temporary file",
    ))
}

/// Rename `work` onto `dest`. Both paths must be in the same directory.
pub(crate) fn replace(work: &Path, dest: &Path) -> io::Result<()> {
    if work.parent() != dest.parent() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "replace requires the same directory",
        ));
    }
    fs::rename(work, dest)
}

/// Create `{prefix}{pid}-{nonce}` with mode 0700. `prefix` may be empty.
pub(crate) fn private_dir(parent: &Path, prefix: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(parent)?;
    for _ in 0..8 {
        let nonce = new_nonce()?;
        let path = parent.join(format!("{prefix}{}-{nonce}", std::process::id()));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                return Ok(path);
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "could not create a private directory",
    ))
}

/// Remove child directories whose trailing `{pid}-{nonce}` pid is dead.
/// Anything that is not a directory, or whose name does not match, is kept.
/// A live pid is kept even if it has been reused; that race is accepted.
pub(crate) fn sweep_stale(parent: &Path) -> io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(removed),
        Err(err) => return Err(err),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(pid) = stale_pid(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        if !pid_is_gone(pid) {
            continue;
        }
        let path = entry.path();
        fs::remove_dir_all(&path)?;
        removed.push(path);
    }
    Ok(removed)
}

fn stale_pid(name: &str) -> Option<u32> {
    let (head, nonce) = name.rsplit_once('-')?;
    if nonce.len() != 32
        || !nonce
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    {
        return None;
    }
    let pid_str = head
        .char_indices()
        .rev()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()
        .map(|(index, _)| &head[index..])?;
    if pid_str.is_empty() || pid_str.len() == head.len() && head.starts_with('0') && head != "0" {
        return None;
    }
    // The digits must be a full hyphen-separated field, or the whole name.
    if let Some(prefix) = head.strip_suffix(pid_str)
        && !prefix.is_empty()
        && !prefix.ends_with('-')
    {
        return None;
    }
    pid_str.parse().ok()
}

fn pid_is_gone(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return false;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            ErrorKind::InvalidInput,
            "path contains an interior NUL byte",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::{Duration, Instant},
    };

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vibe-cache-{}-{}",
            std::process::id(),
            new_nonce().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    struct DirGuard(PathBuf);
    impl Drop for DirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exclusive_temp_conflicts_on_create_new() {
        let dir = DirGuard(scratch());
        let dest = dir.0.join("disk.raw");
        let (_file, path) = exclusive_temp_next_to(&dest).unwrap();
        let err = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("disk.raw.tmp.")
        );
    }

    #[test]
    fn clone_matches_source_and_is_independent() {
        let dir = DirGuard(scratch());
        let src = dir.0.join("src.bin");
        let dest = dir.0.join("dest.bin");
        fs::write(&src, b"alpine-bytes").unwrap();
        let kind = clone_file(&src, &dest).unwrap();
        assert_eq!(kind, CloneKind::Cloned);
        assert_eq!(fs::read(&dest).unwrap(), b"alpine-bytes");
        fs::write(&dest, b"changed").unwrap();
        assert_eq!(fs::read(&src).unwrap(), b"alpine-bytes");
        let err = clone_file(&src, &dest).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
    }

    #[test]
    fn two_threads_contend_for_the_lock() {
        let dir = DirGuard(scratch());
        let path = dir.0.join("alpine.lock");
        let held = FileLock::acquire(&path).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let started = Arc::clone(&barrier);
        let other = path.clone();
        let handle = thread::spawn(move || {
            started.wait();
            let began = Instant::now();
            let lock = FileLock::acquire(&other).unwrap();
            (began.elapsed(), lock)
        });
        barrier.wait();
        thread::sleep(Duration::from_millis(150));
        drop(held);
        let (elapsed, lock) = handle.join().unwrap();
        assert!(
            elapsed >= Duration::from_millis(100),
            "contending acquire returned in {elapsed:?}"
        );
        drop(lock);
    }

    #[test]
    fn existing_disk_lock_is_a_sibling_and_does_not_change_the_disk() {
        let dir = DirGuard(scratch());
        let disk = dir.0.join("disk.raw");
        fs::write(&disk, b"raw").unwrap();
        fs::set_permissions(&disk, fs::Permissions::from_mode(0o644)).unwrap();
        let held = FileLock::try_acquire_existing(&disk).unwrap();
        let lock_path = dir.0.join("disk.raw.lock");
        assert!(lock_path.is_file());
        assert_eq!(
            fs::metadata(&disk).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read(&disk).unwrap(), b"raw");
        let other = disk.clone();
        let handle = thread::spawn(move || {
            let began = Instant::now();
            let result = FileLock::try_acquire_existing(&other);
            (began.elapsed(), result)
        });
        let (elapsed, result) = handle.join().unwrap();
        assert!(matches!(result, Err(LockError::WouldBlock)));
        assert!(elapsed < Duration::from_millis(200), "took {elapsed:?}");
        drop(held);
        assert!(FileLock::try_acquire_existing(&disk).is_ok());

        fs::set_permissions(&disk, fs::Permissions::from_mode(0o444)).unwrap();
        match FileLock::try_acquire_existing(&disk) {
            Err(LockError::Io(err)) => assert_eq!(err.kind(), ErrorKind::PermissionDenied),
            Err(LockError::WouldBlock) => panic!("read-only disk reported WouldBlock"),
            Ok(_) => panic!("read-only disk was locked"),
        }
    }

    #[test]
    fn try_acquire_fails_immediately_when_locked() {
        let dir = DirGuard(scratch());
        let path = dir.0.join("alpine.lock");
        let held = FileLock::acquire(&path).unwrap();
        let other = path.clone();
        let handle = thread::spawn(move || {
            let began = Instant::now();
            let result = FileLock::try_acquire(&other);
            (began.elapsed(), result)
        });
        let (elapsed, result) = handle.join().unwrap();
        assert!(matches!(result, Err(LockError::WouldBlock)));
        assert!(elapsed < Duration::from_millis(200), "took {elapsed:?}");
        drop(held);
        assert!(FileLock::try_acquire(&path).is_ok());
    }

    #[test]
    fn sweep_stale_removes_only_dead_pid_directories() {
        let dir = DirGuard(scratch());
        let nonce = "0123456789abcdef0123456789abcdef";
        let dead_pid = dead_pid();
        let dead = dir.0.join(format!("maint-{dead_pid}-{nonce}"));
        let live = dir.0.join(format!("maint-{}-{nonce}", std::process::id()));
        let notes = dir.0.join("notes");
        let file = dir.0.join(format!("maint-{dead_pid}-{nonce}-file"));
        fs::create_dir(&dead).unwrap();
        fs::create_dir(&live).unwrap();
        fs::create_dir(&notes).unwrap();
        fs::write(&file, b"keep").unwrap();
        fs::write(dead.join("leftover"), b"x").unwrap();

        let removed = sweep_stale(&dir.0).unwrap();
        assert_eq!(removed, vec![dead.clone()]);
        assert!(!dead.exists());
        assert!(live.exists());
        assert!(notes.exists());
        assert!(file.exists());
    }

    #[test]
    fn private_dir_is_exclusive_and_mode_0700() {
        let dir = DirGuard(scratch());
        let created = private_dir(&dir.0, "maint-").unwrap();
        let mode = fs::metadata(&created).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let name = created.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with(&format!("maint-{}-", std::process::id())));
        let err = fs::DirBuilder::new().create(&created).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);

        let work = dir.0.join("work.raw");
        let dest = dir.0.join("dest.raw");
        fs::write(&work, b"disk").unwrap();
        replace(&work, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"disk");
        let elsewhere = created.join("nope.raw");
        fs::write(&dest, b"disk").unwrap();
        assert!(replace(&dest, &elsewhere).is_err());
    }

    fn dead_pid() -> u32 {
        for pid in [2_147_483_646u32, 2_000_000_000, 1_500_000_000] {
            if pid_is_gone(pid) {
                return pid;
            }
        }
        panic!("could not find an unused pid for sweep_stale");
    }
}
