//! Image directories: where named images and the Debian and Alpine base
//! images are looked up, and where downloads and new images are written.
//!
//! The default directory is the cache directory (`$XDG_CACHE_HOME/vibe` or
//! `~/.cache/vibe`). `VIBE_EXTRA_IMAGE_DIR` adds more directories (`:`
//! separated) and `VIBE_PREFERRED_IMAGE_DIR` picks the one that is searched
//! first and receives downloads and new images. VirtioFS shares, the network
//! helper, and maintenance logs always stay in the default directory.
//!
//! Only this module reads the two environment variables.

use std::{
    env,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

pub(crate) const EXTRA_IMAGE_DIR_ENV: &str = "VIBE_EXTRA_IMAGE_DIR";
pub(crate) const PREFERRED_IMAGE_DIR_ENV: &str = "VIBE_PREFERRED_IMAGE_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageDirs {
    /// `$XDG_CACHE_HOME/vibe` or `~/.cache/vibe`. VirtioFS shares and logs stay here.
    default: PathBuf,
    /// Where downloads and new images go.
    preferred: PathBuf,
    /// Lookup order, `preferred` first, no duplicates.
    search: Vec<PathBuf>,
    /// Entries of `search` that did not exist when the config was built.
    missing: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageDirsError {
    var: &'static str,
    entry: String,
    reason: &'static str,
}

impl std::fmt::Display for ImageDirsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[image-dirs] {}: {}: '{}'",
            self.var, self.reason, self.entry
        )
    }
}

impl std::error::Error for ImageDirsError {}

fn error(var: &'static str, entry: &Path, reason: &'static str) -> ImageDirsError {
    ImageDirsError {
        var,
        entry: entry.display().to_string(),
        reason,
    }
}

/// Drop trailing `/`, repeated `/`, and `.` components. `..` is kept.
fn normalize(path: &Path) -> PathBuf {
    path.components()
        .filter(|component| *component != Component::CurDir)
        .collect()
}

fn is_blank(value: &OsStr) -> bool {
    value.as_bytes().iter().all(u8::is_ascii_whitespace)
}

impl ImageDirs {
    /// Only the default directory, as if neither variable were set.
    pub(crate) fn single(default: PathBuf) -> Self {
        Self::from_values(default, None, None).expect("no variables, no errors")
    }

    /// Pure: no environment or file system access. Paths are compared
    /// literally after removing trailing `/`.
    pub(crate) fn from_values(
        default: PathBuf,
        extra: Option<&OsStr>,
        preferred: Option<&OsStr>,
    ) -> Result<Self, ImageDirsError> {
        Self::build(default, extra, preferred, |path| path.to_path_buf())
    }

    /// `key` maps a directory to the identity used for "same directory"
    /// checks (the literal path, or its canonical form when it exists).
    fn build(
        default: PathBuf,
        extra: Option<&OsStr>,
        preferred: Option<&OsStr>,
        key: impl Fn(&Path) -> PathBuf,
    ) -> Result<Self, ImageDirsError> {
        let default = normalize(&default);
        let default_key = key(&default);

        let mut extras: Vec<(PathBuf, PathBuf)> = Vec::new();
        for segment in extra
            .map(|value| value.as_bytes().split(|byte| *byte == b':'))
            .into_iter()
            .flatten()
        {
            let segment = OsStr::from_bytes(segment);
            if is_blank(segment) {
                continue;
            }
            let path = Path::new(segment);
            if !path.is_absolute() {
                return Err(error(EXTRA_IMAGE_DIR_ENV, path, "not an absolute path"));
            }
            let path = normalize(path);
            let path_key = key(&path);
            if path_key == default_key || extras.iter().any(|(_, k)| *k == path_key) {
                continue;
            }
            extras.push((path, path_key));
        }

        let preferred = match preferred.filter(|value| !is_blank(value)) {
            None => None,
            Some(value) => {
                let path = Path::new(value);
                if !path.is_absolute() {
                    return Err(error(PREFERRED_IMAGE_DIR_ENV, path, "not an absolute path"));
                }
                let path = normalize(path);
                let path_key = key(&path);
                if path_key == default_key {
                    None
                } else if let Some(index) = extras.iter().position(|(_, k)| *k == path_key) {
                    Some(index)
                } else {
                    return Err(error(
                        PREFERRED_IMAGE_DIR_ENV,
                        &path,
                        "must be the default image directory or an entry of VIBE_EXTRA_IMAGE_DIR",
                    ));
                }
            }
        };

        let mut search = Vec::with_capacity(extras.len() + 1);
        if let Some(index) = preferred {
            search.push(extras[index].0.clone());
        }
        search.push(default.clone());
        search.extend(
            extras
                .iter()
                .enumerate()
                .filter(|(index, _)| Some(*index) != preferred)
                .map(|(_, (path, _))| path.clone()),
        );
        Ok(Self {
            preferred: search[0].clone(),
            default,
            search,
            missing: Vec::new(),
        })
    }

    /// Read both variables and check them against the file system. A
    /// non-default preferred directory must already exist. Extra directories
    /// that do not exist are kept in the list but marked missing and skipped.
    pub(crate) fn from_env(default: PathBuf) -> Result<Self, ImageDirsError> {
        let extra = env::var_os(EXTRA_IMAGE_DIR_ENV);
        let preferred = env::var_os(PREFERRED_IMAGE_DIR_ENV);
        Self::resolve(default, extra.as_deref(), preferred.as_deref())
    }

    fn resolve(
        default: PathBuf,
        extra: Option<&OsStr>,
        preferred: Option<&OsStr>,
    ) -> Result<Self, ImageDirsError> {
        let key = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut dirs = Self::build(default, extra, preferred, key)?;
        if dirs.preferred != dirs.default && !dirs.preferred.is_dir() {
            return Err(error(
                PREFERRED_IMAGE_DIR_ENV,
                &dirs.preferred,
                "directory does not exist",
            ));
        }
        // The default directory is created at startup, so it is never missing.
        dirs.missing = dirs
            .search
            .iter()
            .filter(|dir| **dir != dirs.default && !dir.is_dir())
            .cloned()
            .collect();
        Ok(dirs)
    }

    pub(crate) fn default_dir(&self) -> &Path {
        &self.default
    }

    pub(crate) fn preferred(&self) -> &Path {
        &self.preferred
    }

    pub(crate) fn search_order(&self) -> &[PathBuf] {
        &self.search
    }

    fn is_missing(&self, dir: &Path) -> bool {
        self.missing.iter().any(|missing| missing == dir)
    }

    /// Directories that are searched, in order, without the missing ones.
    pub(crate) fn existing(&self) -> impl Iterator<Item = &Path> {
        self.search
            .iter()
            .map(PathBuf::as_path)
            .filter(|dir| !self.is_missing(dir))
    }

    /// First existing `<dir>/<file_name>` in search order.
    pub(crate) fn find_file(&self, file_name: &str) -> Option<PathBuf> {
        self.existing()
            .map(|dir| dir.join(file_name))
            .find(|path| path.is_file())
    }

    /// The searched directories, comma separated, for "not found" errors.
    pub(crate) fn searched_list(&self) -> String {
        self.existing()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The numbered "Image directories (lookup order):" block.
    pub(crate) fn describe(&self) -> String {
        let mut text = String::from("Image directories (lookup order):\n");
        for (index, dir) in self.search_order().iter().enumerate() {
            let mut notes = Vec::new();
            if *dir == self.preferred {
                notes.push("preferred");
            }
            if *dir == self.default {
                notes.push("default");
            }
            if self.is_missing(dir) {
                notes.push("missing, skipped");
            }
            text.push_str(&format!("\n  {}. {}", index + 1, dir.display()));
            if !notes.is_empty() {
                text.push_str(&format!(" ({})", notes.join(", ")));
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dirs(extra: Option<&str>, preferred: Option<&str>) -> Result<ImageDirs, ImageDirsError> {
        ImageDirs::from_values(
            PathBuf::from("/home/me/.cache/vibe"),
            extra.map(OsStr::new),
            preferred.map(OsStr::new),
        )
    }

    fn order(dirs: &ImageDirs) -> Vec<&str> {
        dirs.search_order()
            .iter()
            .map(|dir| dir.to_str().unwrap())
            .collect()
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vibe-image-dirs-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    #[test]
    fn unset_variables_search_only_the_default_directory() {
        let dirs = dirs(None, None).unwrap();
        assert_eq!(order(&dirs), ["/home/me/.cache/vibe"]);
        assert_eq!(dirs.preferred(), dirs.default_dir());
    }

    #[test]
    fn extra_directories_follow_the_default_in_written_order() {
        let dirs = dirs(Some("/b:/a"), None).unwrap();
        assert_eq!(order(&dirs), ["/home/me/.cache/vibe", "/b", "/a"]);
        assert_eq!(dirs.preferred(), Path::new("/home/me/.cache/vibe"));
    }

    #[test]
    fn preferred_extra_comes_first_then_default_then_the_rest() {
        let dirs = dirs(Some("/a:/b:/c"), Some("/b")).unwrap();
        assert_eq!(order(&dirs), ["/b", "/home/me/.cache/vibe", "/a", "/c"]);
        assert_eq!(dirs.preferred(), Path::new("/b"));
        assert_eq!(dirs.default_dir(), Path::new("/home/me/.cache/vibe"));
    }

    #[test]
    fn preferred_outside_the_list_is_an_error() {
        let err = dirs(Some("/a"), Some("/elsewhere")).unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with("[image-dirs] VIBE_PREFERRED_IMAGE_DIR:"),
            "{text}"
        );
        assert!(text.contains("/elsewhere"), "{text}");
        assert!(dirs(None, Some("/a")).is_err());
        let default = dirs(Some("/a"), Some("/home/me/.cache/vibe/")).unwrap();
        assert_eq!(order(&default), ["/home/me/.cache/vibe", "/a"]);
    }

    #[test]
    fn blank_preferred_means_the_default_directory() {
        for blank in ["", "  ", "\t"] {
            let dirs = dirs(Some("/a"), Some(blank)).unwrap();
            assert_eq!(order(&dirs), ["/home/me/.cache/vibe", "/a"]);
        }
    }

    #[test]
    fn relative_paths_fail_and_empty_or_duplicate_entries_are_dropped() {
        let err = dirs(Some("/a:images"), None).unwrap_err().to_string();
        assert!(
            err.starts_with("[image-dirs] VIBE_EXTRA_IMAGE_DIR:"),
            "{err}"
        );
        assert!(err.contains("'images'"), "{err}");
        assert!(dirs(Some("/a"), Some("a")).is_err());
        assert!(dirs(Some("~/images"), None).is_err());

        let dirs = dirs(
            Some(":/a/::/b//:/a:/home/me/.cache/vibe/: :/b"),
            Some("/b/"),
        )
        .unwrap();
        assert_eq!(order(&dirs), ["/b", "/home/me/.cache/vibe", "/a"]);
    }

    #[test]
    fn missing_preferred_directory_is_an_error() {
        let root = temp_dir("missing-preferred");
        let absent = root.join("absent");
        let err = ImageDirs::resolve(
            root.join("default"),
            Some(absent.as_os_str()),
            Some(absent.as_os_str()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        // The default directory may be preferred before it is created.
        ImageDirs::resolve(
            root.join("default"),
            None,
            Some(root.join("default").as_os_str()),
        )
        .unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn find_file_skips_missing_directories_and_returns_the_first_match() {
        let root = temp_dir("find");
        let default = root.join("default");
        let a = root.join("a");
        let b = root.join("b");
        let gone = root.join("gone");
        for dir in [&default, &a, &b] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(a.join("same.raw"), b"a").unwrap();
        fs::write(b.join("same.raw"), b"b").unwrap();
        fs::write(b.join("only-b.raw"), b"b").unwrap();
        let extra = format!("{}:{}:{}", gone.display(), a.display(), b.display());
        let dirs = ImageDirs::resolve(
            default.clone(),
            Some(OsStr::new(&extra)),
            Some(b.as_os_str()),
        )
        .unwrap();
        assert_eq!(
            dirs.search_order(),
            [b.clone(), default.clone(), gone.clone(), a.clone()]
        );
        assert_eq!(dirs.find_file("same.raw"), Some(b.join("same.raw")));
        assert_eq!(dirs.find_file("only-b.raw"), Some(b.join("only-b.raw")));
        assert_eq!(dirs.find_file("absent.raw"), None);
        assert!(!dirs.searched_list().contains("gone"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_symlinked_spelling_is_the_same_directory() {
        let root = temp_dir("symlink");
        let default = root.join("default");
        let real = root.join("real");
        let link = root.join("link");
        fs::create_dir_all(&default).unwrap();
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let extra = format!("{}:{}", real.display(), link.display());
        let dirs = ImageDirs::resolve(
            default.clone(),
            Some(OsStr::new(&extra)),
            Some(link.as_os_str()),
        )
        .unwrap();
        assert_eq!(dirs.search_order(), [real.clone(), default.clone()]);
        assert_eq!(dirs.preferred(), real);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn describe_numbers_directories_and_marks_their_roles() {
        let root = temp_dir("describe");
        let default = root.join("default");
        let pref = root.join("pref");
        let gone = root.join("gone");
        fs::create_dir_all(&pref).unwrap();
        let extra = format!("{}:{}", gone.display(), pref.display());
        let dirs = ImageDirs::resolve(
            default.clone(),
            Some(OsStr::new(&extra)),
            Some(pref.as_os_str()),
        )
        .unwrap();
        assert_eq!(
            dirs.describe(),
            format!(
                "Image directories (lookup order):\n  1. {} (preferred)\n  2. {} (default)\n  3. {} (missing, skipped)",
                pref.display(),
                default.display(),
                gone.display()
            )
        );
        assert_eq!(
            ImageDirs::single(default.clone()).describe(),
            format!(
                "Image directories (lookup order):\n  1. {} (preferred, default)",
                default.display()
            )
        );
        let _ = fs::remove_dir_all(&root);
    }
}
