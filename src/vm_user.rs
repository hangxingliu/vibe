//! The guest account named by `--vm-user`.

/// Guest account selected for a run or a provision. Omitting `--vm-user`
/// selects `root`, and `--vm-user root` is the same value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VmUser(String);

impl VmUser {
    pub(crate) const MAX_LEN: usize = 32;

    pub(crate) fn root() -> Self {
        Self("root".to_string())
    }

    /// Accepts `^[a-z_][a-z0-9_-]*$`, length 1–32.
    /// A stricter subset of Debian's `useradd` names, and safe as a single
    /// path component under the host cache directory.
    pub(crate) fn parse(name: &str) -> Result<Self, String> {
        if !is_vm_user_name(name) {
            return Err(format!(
                "--vm-user '{name}' is invalid: use 1-{} characters matching ^[a-z_][a-z0-9_-]*$",
                Self::MAX_LEN
            ));
        }
        Ok(Self(name.to_string()))
    }

    /// The account name. The session still logs in as root, so this is not
    /// read outside tests yet.
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for VmUser {
    fn default() -> Self {
        Self::root()
    }
}

fn is_vm_user_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') || name.len() > VmUser::MAX_LEN {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_root_and_ordinary_names() {
        assert_eq!(VmUser::parse("root").unwrap(), VmUser::root());
        assert_eq!(VmUser::default(), VmUser::root());
        assert_eq!(VmUser::parse("dev").unwrap().as_str(), "dev");
        assert_eq!(VmUser::parse("_a").unwrap().as_str(), "_a");
        assert_eq!(VmUser::parse("dev-user_1").unwrap().as_str(), "dev-user_1");
        let max = format!("a{}", "b".repeat(VmUser::MAX_LEN - 1));
        assert_eq!(max.len(), VmUser::MAX_LEN);
        assert_eq!(VmUser::parse(&max).unwrap().as_str(), max);
    }

    #[test]
    fn rejects_empty_uppercase_digit_slash_and_overlong_names() {
        for name in [
            "",
            "Dev",
            "A",
            "1dev",
            "dev/user",
            "dev.user",
            "-dev",
            "has space",
        ] {
            let err = VmUser::parse(name).unwrap_err();
            assert!(err.contains("--vm-user"), "{name}: {err}");
            assert!(err.contains(name), "{name}: {err}");
        }
        let overlong = "a".repeat(VmUser::MAX_LEN + 1);
        let err = VmUser::parse(&overlong).unwrap_err();
        assert!(err.contains("--vm-user"), "{err}");
        assert!(err.contains(&overlong), "{err}");
    }
}
