//! The text of `vibe --help`.
//!
//! The help is short: one usage block, then one section per option group.
//! The full reference is `USAGE.md` in the repository.

use std::path::Path;

use crate::{BUILTIN_PROVISION_SCRIPTS, image_dirs::ImageDirs};

/// Repository printed by `--help` and `--version`.
pub(crate) const REPO_URL: &str = "https://github.com/hangxingliu/vibe";

/// A left column wider than this gets its own line; its description starts
/// on the next line at the section's description column.
const MAX_LEFT: usize = 40;

/// One option (or environment variable) and its description lines. The
/// renderer does not wrap; each element of `desc` is printed on its own line.
struct Row {
    left: &'static str,
    desc: &'static [&'static str],
}

struct Section {
    title: &'static str,
    rows: &'static [Row],
}

const fn row(left: &'static str, desc: &'static [&'static str]) -> Row {
    Row { left, desc }
}

const SECTIONS: &[Section] = &[
    Section {
        title: "Options (run and provision)",
        rows: &[
            row("-h, --help", &["Print this help."]),
            row(
                "--version",
                &["Print the commit SHA, build date, and repository URL."],
            ),
            row(
                "--image NAME",
                &[
                    "Run: template image (ignored once .vibe/instance.raw exists).",
                    "Provision: image to create (default `default`).",
                ],
            ),
            row("--cpus COUNT", &["Virtual CPUs (default 2)."]),
            row(
                "--ram MB",
                &["RAM in MB (default 2048; 4096 when provisioning)."],
            ),
            row(
                "--vm-user NAME",
                &["Guest account for the session (default root)."],
            ),
            row(
                "--root-password PW",
                &["Root password to log in (default $VIBE_ROOT_PASSWORD)."],
            ),
            row(
                "--[no-]mount-sys-cache",
                &["Mount the shared apt, cargo, yarn, m2, and go caches (on)."],
            ),
            row(
                "--[no-]mount-mise",
                &["Mount the shared mise data at ~/.local/share/mise (on)."],
            ),
        ],
    },
    Section {
        title: "Run options",
        rows: &[
            row(
                "--[no-]mount-cwd",
                &["Share the current directory at ~/<project> (on)."],
            ),
            row(
                "--[no-]mount-tmux-conf",
                &["Share host ~/.tmux and copy ~/.tmux.conf (off)."],
            ),
            row(
                "--[no-]mount-agents-conf",
                &["Mount the shared coding agent configuration (off)."],
            ),
            row(
                "--[no-]write-bashrc",
                &["Write Vibe's ~/.profile and load it (off)."],
            ),
            row(
                "--env NAME",
                &["Export host environment variable NAME in the VM."],
            ),
            row(
                "--mount HOST:GUEST[:MODE]",
                &[
                    "Share HOST at GUEST. MODE: read-write (default)",
                    "or read-only. A relative GUEST is below the vm-user's HOME.",
                ],
            ),
            row(
                "--git MODE",
                &[
                    "How to share .git. MODE: ro (default), rw, or no",
                    "(mask .git with tmpfs).",
                ],
            ),
        ],
    },
    Section {
        title: "Network options (run and provision)",
        rows: &[
            row(
                "--network MODE",
                &["MODE: nat (default, bundled user-mode stack) or vznat."],
            ),
            row(
                "-p, --publish SPEC",
                &[
                    "Forward a guest port to the host (repeatable).",
                    "SPEC: [udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT",
                    "Example: -p 127.0.0.1:8022:22 -p udp:8080:80",
                ],
            ),
            row(
                "--proxy URL",
                &[
                    "Proxy for VM traffic and image downloads",
                    "(http, https, socks5, socks5h; default $https_proxy etc.).",
                ],
            ),
            row(
                "--proxy-bypass LIST",
                &["Destinations reached directly (comma separated)."],
            ),
            row(
                "--[no-]proxy-udp",
                &["Proxy the VM's UDP (nat only; on for SOCKS5)."],
            ),
            row(
                "--[no-]proxy-dns",
                &["Proxy gateway DNS over TCP (nat only; on for SOCKS5)."],
            ),
            row(
                "--[no-]strict-proxy",
                &["Drop VM traffic the proxy cannot carry (nat only; off)."],
            ),
            row(
                "--no-proxy-vm",
                &["Do not proxy VM traffic; downloads still use the proxy."],
            ),
            row(
                "--dns ADDR",
                &["Upstream DNS server for the gateway DNS (repeatable)."],
            ),
        ],
    },
    Section {
        title: "Login actions (run only; in order, in the vm-user's shell, repeatable)",
        rows: &[
            row(
                "--script PATH",
                &["Run a script in the VM; stop if it fails."],
            ),
            row("--send TEXT", &["Type TEXT and a newline."]),
            row(
                "--expect TEXT [SECS]",
                &["Wait for TEXT; shut down with an error after SECS (default 30)."],
            ),
            row(
                "--ssh-key FILE",
                &["Install a public key for the vm-user and start sshd."],
            ),
        ],
    },
    Section {
        title: "Provision options",
        rows: &[
            row("--base NAME|PATH", &["Base image (default Debian stable)."]),
            row(
                "--replace",
                &["Replace an existing image with the same name."],
            ),
            row(
                "--repl",
                &["Open a shell after the scripts run, or boot an existing image."],
            ),
        ],
    },
    Section {
        title: "Environment",
        rows: &[
            row("VIBE_ROOT_PASSWORD", &["Default for --root-password."]),
            row(
                "VIBE_EXTRA_IMAGE_DIR",
                &["Extra image directories, `:` separated."],
            ),
            row(
                "VIBE_PREFERRED_IMAGE_DIR",
                &["Directory searched first; downloads and new images go here."],
            ),
            row(
                "VIBE_KEEP_FAILED_WORK=1",
                &["Keep the work files of a failed provision."],
            ),
        ],
    },
];

/// The full `--help` output, including its final newline.
pub(crate) fn help_text(cache_dir: &Path, image_dirs: &ImageDirs) -> String {
    format!(
        "{}\n  \nCache directory:\n\n  {}\n\n{}\n\n",
        static_help(),
        cache_dir.to_string_lossy(),
        image_dirs.describe(),
    )
}

/// Everything before the dynamic cache and image directory tail.
fn static_help() -> String {
    let mut text = format!(
        "Vibe spins up a Linux VM on Mac to sandbox LLM agents.

Usage:
  vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
  vibe provision [OPTIONS] [PROVISION-OPTIONS] [@built-in | path/to/script.sh ...]

Options may appear before or after `provision`.
Full reference: USAGE.md ({REPO_URL}/blob/main/USAGE.md)
"
    );
    for section in SECTIONS {
        text.push('\n');
        text.push_str(&render_section(section));
    }
    text.push('\n');
    text.push_str(&provisioning_scripts_banner());
    text
}

/// Renders `section` in two columns. The description column is set by the
/// widest left column of this section that fits in `MAX_LEFT`, so each
/// section has its own width.
fn render_section(section: &Section) -> String {
    let width = section
        .rows
        .iter()
        .map(|row| row.left.chars().count())
        .filter(|&len| len <= MAX_LEFT)
        .max()
        .unwrap_or(0);
    let indent = " ".repeat(2 + width + 2);
    let mut text = format!("{}:\n", section.title);
    for row in section.rows {
        let mut desc = row.desc.iter();
        if row.left.chars().count() > MAX_LEFT {
            text.push_str(&format!("  {}\n", row.left));
        } else {
            let first = desc.next().copied().unwrap_or("");
            let line = format!("  {:<width$}  {first}", row.left);
            text.push_str(line.trim_end());
            text.push('\n');
        }
        for line in desc {
            text.push_str(&format!("{indent}{line}\n"));
        }
    }
    text
}

pub(crate) fn provisioning_scripts_banner() -> String {
    let mut scripts: Vec<String> = BUILTIN_PROVISION_SCRIPTS
        .iter()
        .filter(|s| s.name != "base")
        .map(|s| format!("  @{:<10} {}", s.name, s.description))
        .collect();
    scripts.sort();

    format!(
        "Built-in provisioning scripts:

{}",
        scripts.join("\n")
    )
}
