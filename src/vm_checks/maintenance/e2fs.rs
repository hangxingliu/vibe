use std::{
    env, fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
};

use super::disk_read::layout_of;

/// Runs `body` with the device path of the disk's root partition
/// (`/dev/diskNsM`).
///
/// The body's result is stored first, `detach_whole` then runs whether it
/// is `Ok` or `Err`, and only after that is the result returned. There is no
/// `Drop` guard, so a panic inside `body` leaves the disk attached.
pub(super) fn with_root(
    disk: &Path,
    body: impl FnOnce(&str) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let layout = layout_of(disk)?;
    let (whole, _mount) = attach_raw(disk)?;
    let device = format!("{whole}s{}", layout.root.index);
    let result = body(&device);
    detach_whole(&whole);
    result
}

/// Detaches a disk attached by `attach_raw`.
///
/// Tries `hdiutil detach` first and falls back to `diskutil eject` if that
/// fails. Errors are ignored so a cleanup failure never hides the result of
/// the caller.
fn detach_whole(whole: &str) {
    let status = Command::new("hdiutil").args(["detach", whole]).status();
    if status.map(|status| status.success()).unwrap_or(false) {
        return;
    }
    let _ = Command::new("diskutil").args(["eject", whole]).status();
}

/// First whole-disk device in `hdiutil attach -nomount` output.
///
/// A line looks like `/dev/disk4           GUID_partition_scheme`.
/// Partition nodes (`/dev/disk4s1`) are not the whole disk. The device name
/// itself contains the letter `s` in `disk`, so that letter cannot be used
/// to tell a whole disk from a partition.
fn whole_disk_from_attach(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let token = line.split_whitespace().next()?;
        let rest = token.strip_prefix("/dev/disk")?;
        if !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()) {
            Some(token.to_string())
        } else {
            None
        }
    })
}

/// Attaches a raw disk file with `hdiutil attach -nomount` as a
/// `CRawDiskImage`.
///
/// macOS has no loop devices. e2fsprogs tools such as `debugfs` need a
/// block device, and this gives the file a `/dev/diskN` node with one
/// `/dev/diskNsM` node per partition, without mounting anything. Returns the
/// whole-disk node and the full `hdiutil` output. Callers must detach it;
/// `with_root` does that through `detach_whole`.
fn attach_raw(disk: &Path) -> Result<(String, String), Box<dyn std::error::Error>> {
    let output = Command::new("hdiutil")
        .args([
            "attach",
            "-nomount",
            "-imagekey",
            "diskimage-class=CRawDiskImage",
        ])
        .arg(disk)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "hdiutil attach failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let whole = whole_disk_from_attach(&text)
        .ok_or_else(|| format!("hdiutil attach did not name a disk:\n{text}"))?;
    Ok((whole, text))
}

pub(super) fn debugfs_script(
    device: &str,
    commands: &[impl AsRef<str>],
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!("vibe-debugfs-{}.cmds", std::process::id()));
    fs::write(
        &path,
        commands
            .iter()
            .map(|c| c.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )?;
    let output = Command::new(e2fs_bin("debugfs"))
        .args(["-w", "-f"])
        .arg(&path)
        .arg(device)
        .output()?;
    let _ = fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // debugfs exits 0 even when a command in the script fails. The version
    // banner is also on stderr, so match the phrases a failed command prints.
    let command_failed = stderr.contains("File exists")
        || stderr.contains("already exists")
        || stderr.contains("No such file")
        || stderr.contains("File not found")
        || stderr.contains("while ");
    if !output.status.success() || command_failed {
        return Err(format!(
            "debugfs failed: {}{stderr}",
            String::from_utf8_lossy(&output.stdout),
        )
        .into());
    }
    Ok(())
}

pub(super) fn debugfs_out(
    device: &str,
    command: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new(e2fs_bin("debugfs"))
        .args(["-R", command, device])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "debugfs {command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `Flags:` is on the inode line in debugfs 1.47 (`Inode: 11 ... Flags: 0x80000`).
pub(super) fn inode_flags(stat: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let field = stat
        .lines()
        .find_map(|line| {
            let rest = line.split("Flags:").nth(1)?;
            rest.split_whitespace().next().map(str::to_string)
        })
        .ok_or("debugfs stat did not report flags")?;
    let digits = field.trim_start_matches("0x");
    Ok(u32::from_str_radix(digits, 16)?)
}

fn e2fs_bin(name: &str) -> PathBuf {
    if let Ok(dir) = env::var("VIBE_E2FS_SBIN") {
        return PathBuf::from(dir).join(name);
    }
    let bundled = PathBuf::from("/tmp/e2fs-install/sbin").join(name);
    if bundled.exists() {
        bundled
    } else {
        PathBuf::from(name)
    }
}

pub(super) fn set_ext4_label(
    disk: &Path,
    first_lba: u64,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = [0u8; 16];
    let raw = label.as_bytes();
    bytes[..raw.len().min(16)].copy_from_slice(&raw[..raw.len().min(16)]);
    let mut file = fs::OpenOptions::new().write(true).open(disk)?;
    file.seek(SeekFrom::Start(first_lba * 512 + 1024 + 0x78))?;
    file.write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{inode_flags, whole_disk_from_attach};

    #[test]
    fn attach_listing_selects_the_whole_disk() {
        let text = "\
/dev/disk4          \tGUID_partition_scheme          \t\n\
/dev/disk4s1        \tB921B045-1DF0-41C3-AF44-4C6F280\t\n\
/dev/disk4s15       \tEFI\n";
        assert_eq!(whole_disk_from_attach(text).as_deref(), Some("/dev/disk4"));
        assert_eq!(
            whole_disk_from_attach("/dev/disk10\n/dev/disk10s1\tLinux\n").as_deref(),
            Some("/dev/disk10")
        );
        assert_eq!(
            whole_disk_from_attach("/dev/disk4                                          \n")
                .as_deref(),
            Some("/dev/disk4")
        );
        assert_eq!(whole_disk_from_attach("/dev/disk4s1\n").as_deref(), None);
        assert_eq!(whole_disk_from_attach("").as_deref(), None);
    }

    #[test]
    fn debugfs_stat_flags_are_on_the_inode_line() {
        let stat = "\
Inode: 11   Type: directory    Mode:  0700   Flags: 0x80000\n\
Generation: 0    Version: 0x00000000:00000000\n";
        assert_eq!(inode_flags(stat).unwrap(), 0x80000);
        assert!(inode_flags("Inode: 11 Type: directory\n").is_err());
    }
}
