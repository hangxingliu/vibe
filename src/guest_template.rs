//! Marked regions in guest shell text, and shell quoting for values Rust injects.
//!
//! A region is a `# region NAME` line through the matching `# endregion NAME`
//! line. The marker lines stay. `$` in the replacement is copied as text: it
//! is not passed through `Regex::replace`, which would expand `$name` and `$1`.

use base64::{Engine, engine::general_purpose};

use crate::session::{shell_single_quote, shell_word};

/// Why [`replace_region`] refused a template.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RegionError {
    /// No `# region NAME` / `# endregion NAME` pair.
    Missing { name: String },
    /// More than one pair for the same name.
    Duplicate { name: String },
}

impl std::fmt::Display for RegionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { name } => write!(f, "region {name} is missing"),
            Self::Duplicate { name } => write!(f, "region {name} appears more than once"),
        }
    }
}

impl std::error::Error for RegionError {}

/// Replaces the lines between `# region NAME` and `# endregion NAME` with
/// `body`. The marker lines stay. A missing region or a repeated region is
/// an error.
///
/// `body` is inserted exactly. A trailing newline in `body` keeps the end
/// marker on its own line. `$` in `body` is not expanded.
pub(crate) fn replace_region(
    template: &str,
    name: &str,
    body: &str,
) -> Result<String, RegionError> {
    let re = region_regex(name);
    let spans: Vec<(usize, usize)> = re
        .find_iter(template)
        .map(|found| (found.start(), found.end()))
        .collect();
    let (start, end) = match spans.as_slice() {
        [span] => *span,
        [] => {
            return Err(RegionError::Missing {
                name: name.to_string(),
            });
        }
        _ => {
            return Err(RegionError::Duplicate {
                name: name.to_string(),
            });
        }
    };
    let caps = re
        .captures(template)
        .expect("region match has marker groups");
    let start_line = caps.get(1).expect("region start marker").as_str();
    let end_line = caps.get(3).expect("region end marker").as_str();
    let mut out = String::with_capacity(template.len() + body.len());
    out.push_str(&template[..start]);
    out.push_str(start_line);
    out.push_str(body);
    out.push_str(end_line);
    out.push_str(&template[end..]);
    Ok(out)
}

/// Like [`replace_region`], but returns `template` unchanged when the region
/// is missing or repeated. Only for user-editable scripts such as
/// `provisioning/base.sh`.
pub(crate) fn replace_region_if_present(template: &str, name: &str, body: &str) -> String {
    replace_region(template, name, body).unwrap_or_else(|_| template.to_string())
}

/// `NAME='value'` with the value single-quoted for sh.
pub(crate) fn shell_assign(name: &str, value: &str) -> String {
    format!("{name}='{}'", shell_single_quote(value))
}

/// Root shell text that runs `script` in a child `sh` with `args`:
/// `sh -c "$(echo '<b64>' | base64 --decode)" <name> <args>`.
/// `name` and `args` are shell words. stdin stays free, so this is safe
/// before the VirtioFS mount, where the script cannot be read from a file.
pub(crate) fn sh_command(script: &str, name: &str, args: &[String]) -> String {
    let b64 = general_purpose::STANDARD.encode(script);
    let mut command = format!(
        "sh -c \"$(echo '{b64}' | base64 --decode)\" {}",
        shell_word(name)
    );
    for arg in args {
        command.push(' ');
        command.push_str(&shell_word(arg));
    }
    command
}

fn region_regex(name: &str) -> regex::Regex {
    let name = regex::escape(name);
    regex::Regex::new(&format!(
        "(?ms)^([ \\t]*#[ \\t]*region[ \\t]+{name}[ \\t]*\n)(.*?)([ \\t]*#[ \\t]*endregion[ \\t]+{name}[ \\t]*)$"
    ))
    .expect("region marker regex")
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use super::*;

    fn bash_stdout(command: &str, stdin_text: &str) -> String {
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .env("HOME", "/should-not-expand")
            .env("VIBE_VM_HOME", "/should-not-expand")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bash");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin_text.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "status {:?}\nstderr {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn replace_region_keeps_markers_and_outside_text() {
        let template = "before\n  #region DEMO\nold\n  # endregion DEMO\nafter $1\n";
        let updated = replace_region(template, "DEMO", "echo ok\n").unwrap();
        assert_eq!(
            updated,
            "before\n  #region DEMO\necho ok\n  # endregion DEMO\nafter $1\n"
        );
        assert_eq!(
            replace_region_if_present(template, "DEMO", "echo ok\n"),
            updated
        );
    }

    #[test]
    fn an_empty_region_can_be_replaced() {
        let template = "# region EMPTY\n# endregion EMPTY\n";
        assert_eq!(
            replace_region(template, "EMPTY", "x=1\n").unwrap(),
            "# region EMPTY\nx=1\n# endregion EMPTY\n"
        );
    }

    #[test]
    fn injected_text_keeps_dollar_forms() {
        let template = "# region C\nold\n# endregion C\n";
        let body = "echo $HOME ${VIBE_VM_HOME} $1\n";
        assert_eq!(
            replace_region(template, "C", body).unwrap(),
            format!("# region C\n{body}# endregion C\n")
        );
    }

    #[test]
    fn a_missing_or_repeated_region_is_an_error_and_if_present_keeps_the_text() {
        let missing = "no region here\n";
        let only_start = "# region NAME\nstuff\n";
        let twice = "# region NAME\na\n# endregion NAME\n# region NAME\nb\n# endregion NAME\n";
        for template in [missing, only_start] {
            assert!(
                matches!(
                    replace_region(template, "NAME", "x\n"),
                    Err(RegionError::Missing { .. })
                ),
                "{template:?}"
            );
            assert_eq!(replace_region_if_present(template, "NAME", "x\n"), template);
        }
        assert!(matches!(
            replace_region(twice, "NAME", "x\n"),
            Err(RegionError::Duplicate { .. })
        ));
        assert_eq!(replace_region_if_present(twice, "NAME", "x\n"), twice);
    }

    #[test]
    fn a_name_that_is_a_prefix_of_another_does_not_match() {
        let template = "\
# region CONFIG
one
# endregion CONFIG
# region CONFIG_2
two
# endregion CONFIG_2
";
        assert_eq!(
            replace_region(template, "CONFIG", "replaced\n").unwrap(),
            "\
# region CONFIG
replaced
# endregion CONFIG
# region CONFIG_2
two
# endregion CONFIG_2
"
        );
        assert_eq!(
            replace_region(template, "CONFIG_2", "other\n").unwrap(),
            "\
# region CONFIG
one
# endregion CONFIG
# region CONFIG_2
other
# endregion CONFIG_2
"
        );
    }

    #[test]
    fn sh_command_passes_args_and_leaves_stdin_for_the_script() {
        let script = "printf '%s\\n' \"$0\" \"$1\" \"$2\"\ncat\n";
        let args = vec!["a b".to_string(), "c'd".to_string()];
        let command = sh_command(script, "demo.sh", &args);
        let stdout = bash_stdout(&command, "hello-stdin\n");
        assert_eq!(stdout, "demo.sh\na b\nc'd\nhello-stdin\n");
        assert!(!stdout.contains("printf"), "{stdout}");
    }

    #[test]
    fn shell_assign_round_trips_quotes_newlines_and_dollars() {
        let value = "o'clock\n$HOME ${VIBE_VM_HOME} $1";
        let command = format!("{}\nprintf '%s' \"$V\"", shell_assign("V", value));
        assert_eq!(bash_stdout(&command, ""), value);
    }
}
