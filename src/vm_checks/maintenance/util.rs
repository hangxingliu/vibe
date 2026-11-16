use std::{
    env, fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};

use crate::cache::clone_file;

pub(super) fn clone_disk(src: &Path, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    clone_file(src, dest)?;
    fs::set_permissions(dest, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

pub(super) fn require_file(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let value = env::var(name).map_err(|_| format!("{name} is unset"))?;
    let path = PathBuf::from(&value);
    if !path.is_file() {
        return Err(format!("{name} is not a file: {value}").into());
    }
    Ok(path)
}

pub(super) fn work_dir(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = env::temp_dir().join(format!(
        "vibe-check-maint-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&path)?;
    Ok(path)
}

pub(super) fn sha256_file(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("/usr/bin/shasum")
        .args(["--algorithm", "256"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(format!("shasum failed for {}", path.display()).into());
    }
    Ok(String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string())
}

pub(super) fn sha256_range(
    path: &Path,
    offset: u64,
    len: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut child = Command::new("/usr/bin/shasum")
        .args(["--algorithm", "256"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut stdin = child.stdin.take().ok_or("shasum stdin")?;
    let mut left = len;
    let mut buf = vec![0u8; 1024 * 1024];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            return Err("short read while hashing".into());
        }
        stdin.write_all(&buf[..n])?;
        left -= n as u64;
    }
    drop(stdin);
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err("shasum failed".into());
    }
    Ok(String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string())
}

pub(super) struct SavedProxy {
    vars: Vec<(&'static str, Option<String>)>,
}

pub(super) fn proxy_env() -> SavedProxy {
    SavedProxy {
        vars: [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "NO_PROXY",
            "no_proxy",
        ]
        .into_iter()
        .map(|key| (key, env::var(key).ok()))
        .collect(),
    }
}

pub(super) fn set_proxy(proxy: &str) {
    unsafe {
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            env::set_var(key, proxy);
        }
    }
}

pub(super) fn restore_proxy(saved: &SavedProxy) {
    unsafe {
        for (key, value) in &saved.vars {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
    }
}
