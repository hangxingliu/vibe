//! NoCloud seed ISO for one maintenance run.
//!
//! The ISO holds `meta-data`, `user-data` (`maint.sh`), and `manifest`.
//! There is no `policy/` directory. The manifest carries no paths or commands.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

const USER_DATA: &str = include_str!("guest/maint.sh");
const MAX_ISO_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub format: u32,
    pub nonce: String,
    pub alpine_root_uuid: String,
    pub disk_bytes: u64,
    pub root_part: u32,
    pub root_partuuid: String,
    pub root_type: String,
    pub esp_part: u32,
    pub policy: String,
}

#[derive(Debug)]
pub(crate) struct SeedError {
    detail: String,
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for SeedError {}

impl From<io::Error> for SeedError {
    fn from(err: io::Error) -> Self {
        SeedError {
            detail: err.to_string(),
        }
    }
}

impl Manifest {
    /// Key order is fixed. `policy` must be `minimal`. Values are a single token.
    pub(crate) fn serialize(&self) -> Result<String, SeedError> {
        if self.format != 1 {
            return Err(SeedError {
                detail: format!("manifest format {} is not 1", self.format),
            });
        }
        if self.policy != "minimal" {
            return Err(SeedError {
                detail: format!("manifest policy '{}' is not minimal", self.policy),
            });
        }
        if !is_nonce(&self.nonce) {
            return Err(SeedError {
                detail: "manifest nonce is not 32 lowercase hex characters".into(),
            });
        }
        for (key, value) in [
            ("alpine_root_uuid", self.alpine_root_uuid.as_str()),
            ("root_partuuid", self.root_partuuid.as_str()),
            ("root_type", self.root_type.as_str()),
        ] {
            if !is_token(value) {
                return Err(SeedError {
                    detail: format!("manifest {key} contains an illegal character"),
                });
            }
        }
        Ok(format!(
            "format=1\nnonce={}\nalpine_root_uuid={}\ndisk_bytes={}\nroot_part={}\nroot_partuuid={}\nroot_type={}\nesp_part={}\npolicy=minimal\n",
            self.nonce,
            self.alpine_root_uuid,
            self.disk_bytes,
            self.root_part,
            self.root_partuuid,
            self.root_type,
            self.esp_part,
        ))
    }
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

fn is_nonce(value: &str) -> bool {
    value.len() == 32
        && value
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

/// Write the seed directory and `seed.iso` under `dir`. Returns the ISO path.
pub(crate) fn build_seed(dir: &Path, manifest: &Manifest) -> Result<PathBuf, SeedError> {
    let text = manifest.serialize()?;
    if !USER_DATA.starts_with("#!/bin/sh\n") {
        return Err(SeedError {
            detail: "maint.sh does not start with #!/bin/sh".into(),
        });
    }
    let seed_dir = dir.join("seed");
    fs::create_dir_all(&seed_dir)?;
    fs::write(
        seed_dir.join("meta-data"),
        format!(
            "instance-id: vibe-{}\nlocal-hostname: vibe-maint\n",
            manifest.nonce
        ),
    )?;
    fs::write(seed_dir.join("user-data"), USER_DATA)?;
    fs::write(seed_dir.join("manifest"), text)?;
    let iso = dir.join("seed.iso");
    let output = Command::new("hdiutil")
        .args([
            "makehybrid",
            "-quiet",
            "-iso",
            "-joliet",
            "-default-volume-name",
            "cidata",
            "-o",
        ])
        .arg(&iso)
        .arg(&seed_dir)
        .output()
        .map_err(|err| SeedError {
            detail: format!("failed to run hdiutil: {err}"),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SeedError {
            detail: format!("hdiutil makehybrid failed: {stderr}"),
        });
    }
    let len = fs::metadata(&iso)?.len();
    if len == 0 || len >= MAX_ISO_BYTES {
        return Err(SeedError {
            detail: format!("seed.iso is {len} bytes"),
        });
    }
    Ok(iso)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            format: 1,
            nonce: "0123456789abcdef0123456789abcdef".into(),
            alpine_root_uuid: "d51a6213-c765-4b85-9105-1b438c523993".into(),
            disk_bytes: 3221225472,
            root_part: 1,
            root_partuuid: "11111111-2222-3333-4444-555555555555".into(),
            root_type: "b921b045-1df0-41c3-af44-4c6f280d3fae".into(),
            esp_part: 15,
            policy: "minimal".into(),
        }
    }

    #[test]
    fn serialize_uses_the_fixed_field_order() {
        let text = sample().serialize().unwrap();
        assert_eq!(
            text,
            "\
format=1
nonce=0123456789abcdef0123456789abcdef
alpine_root_uuid=d51a6213-c765-4b85-9105-1b438c523993
disk_bytes=3221225472
root_part=1
root_partuuid=11111111-2222-3333-4444-555555555555
root_type=b921b045-1df0-41c3-af44-4c6f280d3fae
esp_part=15
policy=minimal
"
        );
    }

    #[test]
    fn serialize_rejects_illegal_values() {
        let mut manifest = sample();
        manifest.nonce = "zzzz".into();
        assert!(manifest.serialize().is_err());
        manifest = sample();
        manifest.policy = "managed".into();
        assert!(manifest.serialize().is_err());
        manifest = sample();
        manifest.format = 2;
        assert!(manifest.serialize().is_err());
        manifest = sample();
        manifest.root_partuuid = "no/slash".into();
        assert!(manifest.serialize().is_err());
        manifest = sample();
        manifest.alpine_root_uuid = "has space".into();
        assert!(manifest.serialize().is_err());
    }

    #[test]
    fn build_seed_iso_is_cidata_and_starts_with_the_shell() {
        let dir = std::env::temp_dir().join(format!(
            "vibe-seed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        let iso = build_seed(&dir, &sample()).unwrap();
        let info = Command::new("hdiutil")
            .args(["imageinfo"])
            .arg(&iso)
            .output()
            .unwrap();
        let info_text = format!(
            "{}{}",
            String::from_utf8_lossy(&info.stdout),
            String::from_utf8_lossy(&info.stderr)
        );
        assert!(
            info_text.to_ascii_lowercase().contains("cidata"),
            "{info_text}"
        );

        let attached = Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-readonly"])
            .arg(&iso)
            .output()
            .unwrap();
        assert!(
            attached.status.success(),
            "{}",
            String::from_utf8_lossy(&attached.stderr)
        );
        let attach_text = String::from_utf8_lossy(&attached.stdout);
        let mount = attach_text
            .lines()
            .filter_map(|line| {
                line.split('\t')
                    .map(str::trim)
                    .find(|f| f.starts_with("/Volumes"))
            })
            .next()
            .expect("mount point")
            .to_string();
        let user_data = fs::read_to_string(Path::new(&mount).join("user-data")).unwrap();
        assert!(user_data.starts_with("#!/bin/sh\n"), "{user_data:.40}");
        assert!(Path::new(&mount).join("meta-data").is_file());
        assert!(Path::new(&mount).join("manifest").is_file());
        assert!(!Path::new(&mount).join("policy").exists());
        let _ = Command::new("hdiutil").args(["detach", &mount]).status();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shadow_selftest_preserves_every_field_but_the_root_password() {
        let status = Command::new("/bin/sh")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/guest/maint.sh"))
            .arg("shadow-selftest")
            .status()
            .unwrap();
        assert!(status.success(), "shadow selftest failed: {status}");
    }
}
