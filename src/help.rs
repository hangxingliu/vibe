//! The text of `vibe --help`.
//!
//! The help is short: one usage block, then one section per option group.
//! Every two-column section shares one left-column width. On a color
//! terminal the same text is styled, and file paths and URLs are terminal
//! links. Redirected output stays plain. The full reference is `USAGE.md`.

use std::io::{self, IsTerminal};
use std::path::Path;

use crate::{BUILTIN_PROVISION_SCRIPTS, image_dirs::ImageDirs};

/// Repository printed by `--help` and `--version`.
pub(crate) const REPO_URL: &str = "https://github.com/hangxingliu/vibe";

/// A left column wider than this gets its own line; its description starts
/// on the next line at the shared description column.
const MAX_LEFT: usize = 40;

/// Plain text, or ANSI styles and OSC 8 links.
///
/// Both are one switch. `vibe --help > help.txt` is not a terminal, so the
/// file stays plain. `NO_COLOR`, `CLICOLOR=0`, and a terminal without color
/// do the same.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StyleMode {
    Plain,
    Ansi,
}

struct RenderCtx<'a> {
    mode: StyleMode,
    /// Host home, used to turn `~/...` labels into `file://` links.
    home: Option<&'a str>,
}

/// What the terminal and the environment say about color.
struct StyleInput<'a> {
    is_tty: bool,
    term: Option<&'a str>,
    /// `NO_COLOR` is present and not empty (<https://no-color.org/>).
    no_color: bool,
    /// `CLICOLOR=0`.
    clicolor_off: bool,
}

/// One option (or environment variable) and its description lines.
///
/// `left` and each `desc` line use a small markup language. The renderer
/// does not wrap; each element of `desc` is printed on its own line.
///
/// - `{b:text}` bold (the `vibe` command and option names)
/// - `{i:text}` italic (parameter and variable names)
/// - `{d:text}` cyan (a default value)
/// - `{di:text}` italic and cyan (a default that is itself a variable)
/// - `{url:URL}` or `{url:URL|title}` an OSC 8 link
/// - `{file:path}` an OSC 8 `file://` link; `~/...` uses `$HOME` in the URL
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
            row("{b:-h}, {b:--help}", &["Print this help."]),
            row(
                "{b:--version}",
                &["Print the commit SHA, build date, and repository URL."],
            ),
            row(
                "{b:--image} {i:NAME}",
                &[
                    "Run: template image (ignored once .vibe/instance.raw exists).",
                    "Provision: image to create (default `{d:default}`).",
                ],
            ),
            row("{b:--cpus} {i:COUNT}", &["Virtual CPUs (default {d:2})."]),
            row(
                "{b:--ram} {i:MB}",
                &["RAM in MB (default {d:2048}; {d:4096} when provisioning)."],
            ),
            row(
                "{b:--vm-user} {i:NAME}",
                &["Guest account for the session (default {d:root})."],
            ),
            row(
                "{b:--root-password} {i:PW}",
                &["Root password to log in (default {di:$VIBE_ROOT_PASSWORD})."],
            ),
            row(
                "{b:--[no-]mount-sys-cache}",
                &["Mount the shared apt, cargo, yarn, m2, and go caches ({d:on})."],
            ),
            row(
                "{b:--[no-]mount-mise}",
                &["Mount the shared mise data at ~/.local/share/mise ({d:on})."],
            ),
        ],
    },
    Section {
        title: "Run options",
        rows: &[
            row(
                "{b:--[no-]mount-cwd}",
                &["Share the current directory at ~/{i:<project>} ({d:on})."],
            ),
            row(
                "{b:--[no-]mount-tmux-conf}",
                &["Share host {file:~/.tmux} and copy {file:~/.tmux.conf} ({d:off})."],
            ),
            row(
                "{b:--[no-]mount-agents-conf}",
                &["Mount the shared coding agent configuration ({d:off})."],
            ),
            row(
                "{b:--[no-]write-bashrc}",
                &["Write Vibe's ~/.profile and load it ({d:off})."],
            ),
            row(
                "{b:--env} {i:NAME}",
                &["Export host environment variable {i:NAME} in the VM."],
            ),
            row(
                "{b:--mount} {i:HOST:GUEST[:MODE]}",
                &[
                    "Share {i:HOST} at {i:GUEST}. {i:MODE}: {d:read-write} (default)",
                    "or read-only. A relative {i:GUEST} is below the vm-user's {i:HOME}.",
                ],
            ),
            row(
                "{b:--git} {i:MODE}",
                &[
                    "How to share .git. {i:MODE}: {d:ro} (default), rw, or no",
                    "(mask .git with tmpfs).",
                ],
            ),
        ],
    },
    Section {
        title: "Network options (run and provision)",
        rows: &[
            row(
                "{b:--network} {i:MODE}",
                &["{i:MODE}: {d:nat} (default, bundled user-mode stack) or vznat."],
            ),
            row(
                "{b:-p}, {b:--publish} {i:SPEC}",
                &[
                    "Forward a guest port to the host (repeatable).",
                    "{i:SPEC}: {i:[udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT}",
                    "Example: {b:-p} 127.0.0.1:8022:22 {b:-p} udp:8080:80",
                ],
            ),
            row(
                "{b:--proxy} {i:URL}",
                &[
                    "Proxy for VM traffic and image downloads",
                    "(http, https, socks5, socks5h; default {di:$https_proxy} etc.).",
                ],
            ),
            row(
                "{b:--proxy-bypass} {i:LIST}",
                &["Destinations reached directly (comma separated)."],
            ),
            row(
                "{b:--[no-]proxy-udp}",
                &["Proxy the VM's UDP (nat only; {d:on} for SOCKS5)."],
            ),
            row(
                "{b:--[no-]proxy-dns}",
                &["Proxy gateway DNS over TCP (nat only; {d:on} for SOCKS5)."],
            ),
            row(
                "{b:--[no-]strict-proxy}",
                &["Drop VM traffic the proxy cannot carry (nat only; {d:off})."],
            ),
            row(
                "{b:--no-proxy-vm}",
                &["Do not proxy VM traffic; downloads still use the proxy."],
            ),
            row(
                "{b:--dns} {i:ADDR}",
                &["Upstream DNS server for the gateway DNS (repeatable)."],
            ),
        ],
    },
    Section {
        title: "Login actions (run only; in order, in the vm-user's shell, repeatable)",
        rows: &[
            row(
                "{b:--script} {i:PATH}",
                &["Run a script in the VM; stop if it fails."],
            ),
            row("{b:--send} {i:TEXT}", &["Type {i:TEXT} and a newline."]),
            row(
                "{b:--expect} {i:TEXT} {i:[SECS]}",
                &[
                    "Wait for {i:TEXT}; shut down with an error after",
                    "{i:SECS} (default {d:30}).",
                ],
            ),
            row(
                "{b:--ssh-key} {i:FILE}",
                &["Install a public key for the vm-user and start sshd."],
            ),
        ],
    },
    Section {
        title: "Provision options",
        rows: &[
            row(
                "{b:--base} {i:NAME|PATH}",
                &["Base image (default {d:Debian stable})."],
            ),
            row(
                "{b:--replace}",
                &["Replace an existing image with the same name."],
            ),
            row(
                "{b:--repl}",
                &[
                    "Open a shell after the scripts run,",
                    "or boot an existing image.",
                ],
            ),
        ],
    },
    Section {
        title: "Environment",
        rows: &[
            row(
                "{i:VIBE_ROOT_PASSWORD}",
                &["Default for {b:--root-password}."],
            ),
            row(
                "{i:VIBE_EXTRA_IMAGE_DIR}",
                &["Extra image directories, `:` separated."],
            ),
            row(
                "{i:VIBE_PREFERRED_IMAGE_DIR}",
                &["Directory searched first; downloads and new images go here."],
            ),
            row(
                "{i:VIBE_KEEP_FAILED_WORK}=1",
                &["Keep the work files of a failed provision."],
            ),
        ],
    },
];

/// The full `--help` output, including its final newline.
///
/// Styles and links are included only when stdout is a color terminal.
pub(crate) fn help_text(cache_dir: &Path, image_dirs: &ImageDirs) -> String {
    let home = std::env::var("HOME").ok();
    let ctx = RenderCtx {
        mode: detect_style_mode(),
        home: home.as_deref(),
    };
    render_help(cache_dir, image_dirs, &ctx)
}

fn detect_style_mode() -> StyleMode {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    let clicolor_off = std::env::var("CLICOLOR").ok().as_deref() == Some("0");
    let term = std::env::var("TERM").ok();
    style_mode(StyleInput {
        is_tty: io::stdout().is_terminal(),
        term: term.as_deref(),
        no_color,
        clicolor_off,
    })
}

fn style_mode(input: StyleInput<'_>) -> StyleMode {
    if !input.is_tty || input.no_color || input.clicolor_off {
        return StyleMode::Plain;
    }
    match input.term {
        Some(term) if term_supports_color(term) => StyleMode::Ansi,
        _ => StyleMode::Plain,
    }
}

/// `dumb`, `unknown`, and monochrome names (`xterm-mono`, `*-m`) have no color.
fn term_supports_color(term: &str) -> bool {
    let term = term.trim();
    if term.is_empty() || term.eq_ignore_ascii_case("dumb") || term.eq_ignore_ascii_case("unknown")
    {
        return false;
    }
    let lower = term.to_ascii_lowercase();
    !lower.ends_with("-mono") && !lower.ends_with("-m")
}

fn plain_ctx() -> RenderCtx<'static> {
    RenderCtx {
        mode: StyleMode::Plain,
        home: None,
    }
}

fn render_help(cache_dir: &Path, image_dirs: &ImageDirs, ctx: &RenderCtx<'_>) -> String {
    let cache_path = cache_dir.to_string_lossy();
    format!(
        "{}\n  \n{}\n\n  {}\n\n{}\n\n",
        static_help(ctx),
        styled_title("Cache directory:", ctx.mode),
        file_link(&cache_path, &cache_path, ctx),
        style_image_dirs(&image_dirs.describe(), ctx),
    )
}

/// Everything before the dynamic cache and image directory tail.
fn static_help(ctx: &RenderCtx<'_>) -> String {
    let usage_url = format!("{REPO_URL}/blob/main/USAGE.md");
    let run_usage = render_inline(
        "{b:vibe} {i:[OPTIONS]} {i:[LOGIN-ACTIONS ...]} {i:[path/to/disk.raw]}",
        ctx,
    );
    let provision_usage = render_inline(
        "{b:vibe} provision {i:[OPTIONS]} {i:[PROVISION-OPTIONS]} {i:[@built-in | path/to/script.sh ...]}",
        ctx,
    );
    let usage_name = render_inline(&format!("{{url:{usage_url}|USAGE.md}}"), ctx);
    let usage_link = render_inline(&format!("{{url:{usage_url}}}"), ctx);
    let mut text = format!(
        "\
Vibe spins up a Linux VM on Mac to sandbox LLM agents.

{title}
  {run_usage}
  {provision_usage}

Options may appear before or after `provision`.
Full reference: {usage_name} ({usage_link})
",
        title = styled_title("Usage:", ctx.mode),
    );
    let width = option_column_width();
    for section in SECTIONS {
        text.push('\n');
        text.push_str(&render_section(section, width, ctx));
    }
    text.push('\n');
    text.push_str(&render_scripts(ctx));
    text
}

/// Widest left column that still fits on the option line.
///
/// Every two-column section uses this width, including Environment, so the
/// description column lines up from `Options (run and provision)` through
/// `Run options` and the rest.
fn option_column_width() -> usize {
    SECTIONS
        .iter()
        .flat_map(|section| section.rows.iter())
        .map(|row| plain_width(row.left))
        .filter(|len| *len <= MAX_LEFT)
        .max()
        .unwrap_or(0)
}

/// Renders `section` in two columns at the shared `width`.
fn render_section(section: &Section, width: usize, ctx: &RenderCtx<'_>) -> String {
    let indent = " ".repeat(2 + width + 2);
    let mut text = styled_title(&format!("{}:", section.title), ctx.mode);
    text.push('\n');
    for row in section.rows {
        let visible_left = plain_text(row.left);
        let styled_left = render_inline(row.left, ctx);
        let mut desc = row.desc.iter();
        if visible_left.chars().count() > MAX_LEFT {
            text.push_str("  ");
            text.push_str(&styled_left);
            text.push('\n');
        } else {
            let first = desc.next().copied().unwrap_or("");
            let pad = width - visible_left.chars().count();
            let mut line = String::new();
            line.push_str("  ");
            line.push_str(&styled_left);
            line.push_str(&" ".repeat(pad));
            line.push_str("  ");
            line.push_str(&render_inline(first, ctx));
            text.push_str(line.trim_end());
            text.push('\n');
        }
        for line in desc {
            text.push_str(&indent);
            text.push_str(&render_inline(line, ctx));
            text.push('\n');
        }
    }
    text
}

pub(crate) fn provisioning_scripts_banner() -> String {
    render_scripts(&plain_ctx())
}

fn render_scripts(ctx: &RenderCtx<'_>) -> String {
    let mut scripts: Vec<String> = BUILTIN_PROVISION_SCRIPTS
        .iter()
        .filter(|script| script.name != "base")
        .map(|script| format!("  @{:<10} {}", script.name, script.description))
        .collect();
    scripts.sort();
    format!(
        "{}\n\n{}",
        styled_title("Built-in provisioning scripts:", ctx.mode),
        scripts.join("\n")
    )
}

fn styled_title(title: &str, mode: StyleMode) -> String {
    match mode {
        StyleMode::Plain => title.to_string(),
        StyleMode::Ansi => match title.split_once('(') {
            Some((prefix, suffix)) => format!("{} ({suffix}", sgr("1;4", prefix.trim_end())),
            None => sgr("1;4", title),
        },
    }
}

/// Cyan (ANSI 36) marks a default value. It stays readable on light and dark
/// backgrounds and is distinct from the bold headings.
fn sgr(code: &str, text: &str) -> String {
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

fn plain_width(markup: &str) -> usize {
    plain_text(markup).chars().count()
}

fn plain_text(markup: &str) -> String {
    render_inline(markup, &plain_ctx())
}

fn render_inline(input: &str, ctx: &RenderCtx<'_>) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        if rest[open..].starts_with("{{") {
            out.push('{');
            rest = &rest[open + 2..];
            continue;
        }
        let body_start = open + 1;
        let Some(close) = rest[body_start..].find('}') else {
            panic!("unclosed help markup in {input:?}");
        };
        let body = &rest[body_start..body_start + close];
        rest = &rest[body_start + close + 1..];
        let Some((tag, text)) = body.split_once(':') else {
            panic!("help markup needs a tag: {{{body}}}");
        };
        out.push_str(&render_tag(tag, text, ctx));
    }
    out.push_str(rest);
    out
}

fn render_tag(tag: &str, text: &str, ctx: &RenderCtx<'_>) -> String {
    match tag {
        "b" => styled_span("1", text, ctx),
        "i" => styled_span("3", text, ctx),
        "d" => styled_span("36", text, ctx),
        "di" => styled_span("3;36", text, ctx),
        "url" => {
            let (url, title) = text.split_once('|').unwrap_or((text, text));
            url_link(url, title, ctx)
        }
        "file" => file_link(text, text, ctx),
        other => panic!("unknown help markup tag {other}"),
    }
}

fn styled_span(code: &str, text: &str, ctx: &RenderCtx<'_>) -> String {
    match ctx.mode {
        StyleMode::Plain => text.to_string(),
        StyleMode::Ansi => sgr(code, text),
    }
}

/// OSC 8 hyperlink. The label is `title`; the target is `url`.
///
/// This is the sequence `\x1b]8;;URL\x1b\\TITLE\x1b]8;;\x1b\\`.
fn terminal_url(url: &str, title: &str) -> String {
    format!("\u{1b}]8;;{url}\u{1b}\\{title}\u{1b}]8;;\u{1b}\\")
}

fn url_link(url: &str, title: &str, ctx: &RenderCtx<'_>) -> String {
    match ctx.mode {
        StyleMode::Plain => title.to_string(),
        StyleMode::Ansi => terminal_url(url, title),
    }
}

/// `file://` link. `~/...` keeps that label and points at `$HOME/...`.
/// A relative path, or a tilde path with no home, is plain text.
fn file_link(path: &str, title: &str, ctx: &RenderCtx<'_>) -> String {
    if ctx.mode == StyleMode::Plain {
        return title.to_string();
    }
    match absolute_file_path(path, ctx.home) {
        Some(absolute) => terminal_url(&file_url(&absolute), title),
        None => title.to_string(),
    }
}

fn absolute_file_path(path: &str, home: Option<&str>) -> Option<String> {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = home.filter(|home| !home.is_empty())?;
        let home = home.trim_end_matches('/');
        if home.is_empty() {
            return Some(format!("/{rest}"));
        }
        return Some(format!("{home}/{rest}"));
    }
    path.starts_with('/').then(|| path.to_string())
}

/// Percent-encode the path so a space still forms one OSC 8 URI.
/// The visible label stays the raw path.
fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                url.push(byte as char);
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

fn style_image_dirs(described: &str, ctx: &RenderCtx<'_>) -> String {
    if ctx.mode == StyleMode::Plain {
        return described.to_string();
    }
    let mut out = String::new();
    for (index, line) in described.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if index == 0 {
            out.push_str(&styled_title(line, ctx.mode));
        } else if !line.is_empty() {
            out.push_str(&link_directory_line(line, ctx));
        }
    }
    out
}

/// Link the absolute path on a `describe` line. Role notes such as
/// `(preferred, default)` stay outside the link.
fn link_directory_line(line: &str, ctx: &RenderCtx<'_>) -> String {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let (number, payload) = split_numbered_prefix(rest).unwrap_or(("", rest));
    let (path, notes) = split_dir_notes(payload);
    format!(
        "{}{number}{}{notes}",
        &line[..indent],
        file_link(path, path, ctx)
    )
}

fn split_numbered_prefix(rest: &str) -> Option<(&str, &str)> {
    let mut digits = 0;
    for byte in rest.bytes() {
        if byte.is_ascii_digit() {
            digits += 1;
        } else {
            break;
        }
    }
    if digits == 0 {
        return None;
    }
    let payload = rest[digits..].strip_prefix(". ")?;
    Some((&rest[..digits + 2], payload))
}

fn split_dir_notes(payload: &str) -> (&str, &str) {
    let Some(pos) = payload.rfind(" (") else {
        return (payload, "");
    };
    let notes = &payload[pos..];
    if !notes.ends_with(')') {
        return (payload, "");
    }
    let inner = &notes[2..notes.len() - 1];
    let words = inner
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ',' || c == ' ');
    if !words || !inner.chars().any(|c| c.is_ascii_alphanumeric()) {
        return (payload, "");
    }
    (&payload[..pos], notes)
}
