//! Proxy settings shared by the VM network and host downloads: the host's
//! proxy environment variables, proxy URL normalization and redaction, the
//! bypass (NO_PROXY) list, the curl arguments for host downloads, and the
//! resolution of the VM's proxy configuration with its startup summary.
//!
//! The functions here are pure; the environment is passed in through
//! [`HostProxyEnv`] so the rules can be tested.

use std::{
    net::{IpAddr, Ipv4Addr},
    sync::OnceLock,
};

use crate::networking::NetworkMode;

/// Proxy environment variables, in priority order. `ALL_PROXY` is not read.
const PROXY_ENV_VARS: [&str; 4] = ["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"];
/// Bypass list environment variables, in priority order.
const NO_PROXY_ENV_VARS: [&str; 2] = ["no_proxy", "NO_PROXY"];

const CLI_PROXY_SOURCE: &str = "--proxy";
const CLI_BYPASS_SOURCE: &str = "--proxy-bypass";

/// The NAT target that is always reached directly, for the summary.
const GATEWAY_NOTE: &str = "gateway 192.168.5.2 -> host is always direct";

/// Where provisioning in `vznat` mode saves the proxy.
pub(crate) const APT_PROXY_FILE: &str = "/etc/apt/apt.conf.d/99timeout";

/// Text form of the built-in bypass list used when no bypass list is set.
///
/// Keep in sync with `bypassCIDRs` and `bypassHosts` in
/// `helpers/gvisor-tap-vsock/pkg/services/noproxy/noproxy.go`.
pub(crate) fn builtin_bypass_text() -> &'static str {
    "0.0.0.0/8,10.0.0.0/8,127.0.0.0/8,169.254.0.0/16,172.16.0.0/12,192.168.0.0/16,\
100.64.0.0/10,192.0.0.0/24,198.18.0.0/15,224.0.0.0/4,255.255.255.255/32,\
::1/128,fc00::/7,fe80::/10,ff00::/8,localhost,localhost.localdomain"
}

/// The proxy-related environment variables of the host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostProxyEnv {
    /// The first non-empty proxy variable and its name.
    pub proxy: Option<(String, &'static str)>,
    /// The first non-empty bypass list variable and its name.
    pub no_proxy: Option<(String, &'static str)>,
}

impl HostProxyEnv {
    pub(crate) fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let first = |names: &[&'static str]| {
            names.iter().find_map(|&name| {
                lookup(name)
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| (value.trim().to_string(), name))
            })
        };
        Self {
            proxy: first(&PROXY_ENV_VARS),
            no_proxy: first(&NO_PROXY_ENV_VARS),
        }
    }

    pub(crate) fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProxyKind {
    /// `http://` or `https://` (HTTP CONNECT).
    Http,
    /// `socks5://` or `socks5h://`.
    Socks5,
}

/// A proxy URL with an explicit scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedProxy {
    /// The URL for the VM network helper (`socks5h://` becomes `socks5://`).
    pub vm_url: String,
    /// The URL for curl and the guest (`socks5h://` is kept).
    pub curl_url: String,
    pub kind: ProxyKind,
}

/// Adds `http://` when the scheme is missing and checks the scheme.
/// SOCKS versions other than 5 and unknown schemes are errors. Credentials
/// in the URL are redacted in the error.
pub(crate) fn normalize_proxy_url(raw: &str) -> Result<NormalizedProxy, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("the proxy URL is empty".to_string());
    }
    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("http".to_string(), raw),
    };
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    if host.is_empty() || host.starts_with(':') {
        return Err(format!(
            "the proxy URL {} has no host",
            redact_proxy_url(raw)
        ));
    }
    let (kind, vm_scheme) = match scheme.as_str() {
        "http" | "https" => (ProxyKind::Http, scheme.as_str()),
        "socks5" | "socks5h" => (ProxyKind::Socks5, "socks5"),
        "socks" | "socks4" | "socks4a" => {
            return Err(format!(
                "the proxy URL {} uses SOCKS version 4; only socks5:// (or socks5h://) is supported",
                redact_proxy_url(raw)
            ));
        }
        _ => {
            return Err(format!(
                "the proxy URL {} has an unsupported scheme; use http://, https://, socks5://, or socks5h://",
                redact_proxy_url(raw)
            ));
        }
    };
    Ok(NormalizedProxy {
        vm_url: format!("{vm_scheme}://{rest}"),
        curl_url: format!("{scheme}://{rest}"),
        kind,
    })
}

/// Replaces `user:pass@` in a proxy URL with `***@`.
pub(crate) fn redact_proxy_url(url: &str) -> String {
    let (prefix, rest) = match url.find("://") {
        Some(index) => url.split_at(index + 3),
        None => ("", url),
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    match authority.rfind('@') {
        Some(at) => format!("{prefix}***{}{path}", &authority[at..]),
        None => url.to_string(),
    }
}

/// Which consumer of the bypass list a warning is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WarningScope {
    Vm,
    Downloads,
    Both,
}

/// A parsed bypass list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BypassList {
    raw: String,
    source: String,
    canonical: Vec<String>,
    curl: Vec<String>,
    warnings: Vec<(WarningScope, String)>,
}

impl BypassList {
    /// The canonical list for `vibe-usernet --proxy-bypass`: `*`, CIDRs,
    /// `CIDR:port` (IPv4), and `[CIDR]:port`.
    pub(crate) fn usernet_arg(&self) -> String {
        self.canonical.join(",")
    }

    /// The list for `curl --noproxy`.
    pub(crate) fn curl_noproxy(&self) -> String {
        self.curl.join(",")
    }

    /// The list as written, for the summary.
    pub(crate) fn display(&self) -> &str {
        &self.raw
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    /// Warnings that apply to VM traffic.
    pub(crate) fn vm_warnings(&self) -> impl Iterator<Item = &str> {
        self.warnings_for(WarningScope::Vm)
    }

    /// Warnings that apply to host downloads.
    pub(crate) fn download_warnings(&self) -> impl Iterator<Item = &str> {
        self.warnings_for(WarningScope::Downloads)
    }

    fn warnings_for(&self, scope: WarningScope) -> impl Iterator<Item = &str> {
        self.warnings
            .iter()
            .filter(move |(s, _)| *s == scope || *s == WarningScope::Both)
            .map(|(_, warning)| warning.as_str())
    }
}

/// Parses a NO_PROXY-style list. Items are separated by `,`, trimmed, and
/// matched case-insensitively. Problems are warnings, never errors; each
/// names the item and `source`.
pub(crate) fn parse_bypass_list(list: &str, source: &str) -> BypassList {
    let mut out = BypassList {
        raw: list.trim().to_string(),
        source: source.to_string(),
        canonical: Vec::new(),
        curl: Vec::new(),
        warnings: Vec::new(),
    };
    let push = |list: &mut Vec<String>, item: String| {
        if !list.contains(&item) {
            list.push(item);
        }
    };

    for item in list.split(',') {
        let item = item.trim().to_ascii_lowercase();
        if item.is_empty() {
            continue;
        }
        if item == "*" {
            push(&mut out.canonical, "*".to_string());
            push(&mut out.curl, "*".to_string());
            out.warnings.push((
                WarningScope::Both,
                format!("{source} item \"*\" bypasses the proxy for every destination"),
            ));
            continue;
        }

        let Some((host, port)) = split_port(&item) else {
            out.warnings
                .push((WarningScope::Both, unparseable(&item, source)));
            continue;
        };

        let cidrs = if host == "localhost" || host == "localhost.localdomain" {
            Some(vec!["127.0.0.0/8".to_string(), "::1/128".to_string()])
        } else {
            parse_cidr(host)
                .or_else(|| parse_ipv4_wildcard(host))
                .map(|cidr| vec![cidr])
        };

        if let Some(cidrs) = cidrs {
            for cidr in cidrs {
                let canonical = match port {
                    None => cidr,
                    Some(port) if cidr.contains(':') => format!("[{cidr}]:{port}"),
                    Some(port) => format!("{cidr}:{port}"),
                };
                push(&mut out.canonical, canonical);
            }
            if port.is_some() {
                out.warnings.push((
                    WarningScope::Downloads,
                    format!(
                        "{source} item \"{item}\" has a port; host downloads (curl) do not support ports in the bypass list, so it only applies to VM traffic"
                    ),
                ));
            } else if parse_ipv4_wildcard(host).is_some() {
                push(&mut out.curl, parse_ipv4_wildcard(host).unwrap());
            } else {
                push(&mut out.curl, host.to_string());
            }
            continue;
        }

        if is_domain(host) {
            if port.is_some() {
                out.warnings.push((
                    WarningScope::Both,
                    format!(
                        "ignoring {source} item \"{item}\": a domain with a port is not supported"
                    ),
                ));
                continue;
            }
            let curl_form = host.strip_prefix('*').unwrap_or(host).to_string();
            push(&mut out.curl, curl_form);
            out.warnings.push((
                WarningScope::Vm,
                format!(
                    "{source} item \"{item}\" is a domain; VM traffic is matched by IP only, so it only applies to host downloads"
                ),
            ));
            continue;
        }

        out.warnings
            .push((WarningScope::Both, unparseable(&item, source)));
    }
    out
}

fn unparseable(item: &str, source: &str) -> String {
    format!(
        "ignoring {source} item \"{item}\": it is not an IP address, CIDR, IPv4 wildcard, or domain"
    )
}

/// Splits `host:port` and `[host]:port`. A bare IPv6 address has no port.
/// Returns `None` for a malformed port or brackets.
fn split_port(item: &str) -> Option<(&str, Option<u16>)> {
    if let Some(rest) = item.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        if after.is_empty() {
            return Some((host, None));
        }
        let port = parse_port(after.strip_prefix(':')?)?;
        return Some((host, Some(port)));
    }
    match item.matches(':').count() {
        1 => {
            let (host, port) = item.split_once(':')?;
            Some((host, Some(parse_port(port)?)))
        }
        _ => Some((item, None)),
    }
}

fn parse_port(port: &str) -> Option<u16> {
    port.parse::<u16>().ok().filter(|port| *port != 0)
}

/// An IP address or CIDR, as a CIDR.
fn parse_cidr(host: &str) -> Option<String> {
    let (addr, prefix) = match host.split_once('/') {
        Some((addr, prefix)) => (addr, Some(prefix)),
        None => (host, None),
    };
    let ip: IpAddr = addr.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(prefix) => prefix.parse::<u8>().ok().filter(|p| *p <= max)?,
        None => max,
    };
    Some(format!("{ip}/{prefix}"))
}

/// `192.168.*` and `10.*` style IPv4 wildcards, as a CIDR.
fn parse_ipv4_wildcard(host: &str) -> Option<String> {
    let mut octets: Vec<&str> = host.split('.').collect();
    if octets.last() != Some(&"*") {
        return None;
    }
    while octets.last() == Some(&"*") {
        octets.pop();
    }
    if octets.is_empty() || octets.len() > 3 {
        return None;
    }
    let mut bytes = [0u8; 4];
    for (i, octet) in octets.iter().enumerate() {
        bytes[i] = octet.parse().ok()?;
    }
    Some(format!("{}/{}", Ipv4Addr::from(bytes), octets.len() * 8))
}

/// `example.com`, `.example.com`, and `*.example.com`.
fn is_domain(host: &str) -> bool {
    let name = host
        .strip_prefix("*.")
        .or_else(|| host.strip_prefix('.'))
        .unwrap_or(host);
    !name.is_empty()
        && name.chars().any(|c| c.is_ascii_alphabetic())
        && name.split('.').all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// The bypass list from `--proxy-bypass`, `no_proxy`, or `NO_PROXY`, in that
/// order. `None` means the built-in list.
fn bypass_from(cli_bypass: Option<&str>, env: &HostProxyEnv) -> Option<BypassList> {
    match (cli_bypass, &env.no_proxy) {
        (Some(list), _) => Some(parse_bypass_list(list, CLI_BYPASS_SOURCE)),
        (None, Some((list, name))) => Some(parse_bypass_list(list, name)),
        (None, None) => None,
    }
}

/// The proxy from `--proxy` or the environment, with its source.
fn proxy_from(
    cli_proxy: Option<&str>,
    env: &HostProxyEnv,
) -> Result<Option<(NormalizedProxy, &'static str)>, String> {
    match (cli_proxy, &env.proxy) {
        (Some(url), _) => normalize_proxy_url(url)
            .map(|proxy| Some((proxy, CLI_PROXY_SOURCE)))
            .map_err(|err| format!("invalid --proxy: {err}")),
        (None, Some((url, name))) => normalize_proxy_url(url)
            .map(|proxy| Some((proxy, *name)))
            .map_err(|err| format!("invalid proxy in the {name} environment variable: {err}")),
        (None, None) => Ok(None),
    }
}

/// The proxy and bypass list for curl when downloading images on the host.
/// Only `--proxy`, `--proxy-bypass`, and the environment are used; the
/// VM-only options do not apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DownloadProxy {
    proxy: Option<(NormalizedProxy, &'static str)>,
    bypass: Option<BypassList>,
}

impl DownloadProxy {
    pub(crate) fn resolve(
        cli_proxy: Option<&str>,
        cli_bypass: Option<&str>,
        env: &HostProxyEnv,
    ) -> Result<Self, String> {
        Ok(Self {
            proxy: proxy_from(cli_proxy, env)?,
            bypass: bypass_from(cli_bypass, env),
        })
    }

    /// `--proxy URL` (or `--proxy ""`, so curl reads no proxy variables),
    /// then `--noproxy LIST` when a bypass list is set.
    pub(crate) fn curl_args(&self) -> Vec<String> {
        let mut args = vec![
            "--proxy".to_string(),
            self.proxy
                .as_ref()
                .map(|(proxy, _)| proxy.curl_url.clone())
                .unwrap_or_default(),
        ];
        if let Some(bypass) = &self.bypass {
            args.push("--noproxy".to_string());
            args.push(bypass.curl_noproxy());
        }
        args
    }

    /// The `Host downloads proxy: ...` line, followed by `warning: ` lines.
    pub(crate) fn describe(&self) -> String {
        let mut out = match &self.proxy {
            Some((proxy, source)) => format!(
                "Host downloads proxy: {} (from {source})",
                redact_proxy_url(&proxy.curl_url)
            ),
            None => "Host downloads proxy: none".to_string(),
        };
        if let Some(bypass) = &self.bypass {
            out.push_str(&format!(
                "; bypass {} (from {})",
                bypass.curl_noproxy(),
                bypass.source()
            ));
            for warning in bypass.download_warnings() {
                out.push_str(&format!("\nwarning: {warning}"));
            }
        }
        out
    }
}

static DOWNLOAD_PROXY: OnceLock<Result<DownloadProxy, String>> = OnceLock::new();

/// Sets the download proxy from the command line. Called once at startup.
pub(crate) fn set_download_proxy(proxy: Result<DownloadProxy, String>) {
    let _ = DOWNLOAD_PROXY.set(proxy);
}

/// The download proxy set at startup, or one read from the current
/// environment when none was set (VM checks and tests).
pub(crate) fn download_proxy() -> Result<DownloadProxy, String> {
    match DOWNLOAD_PROXY.get() {
        Some(proxy) => proxy.clone(),
        None => DownloadProxy::resolve(None, None, &HostProxyEnv::from_env()),
    }
}

/// The proxy-related command line options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct VmProxyArgs<'a> {
    pub proxy: Option<&'a str>,
    pub proxy_bypass: Option<&'a str>,
    pub proxy_udp: Option<bool>,
    pub proxy_dns: Option<bool>,
    pub strict_proxy: Option<bool>,
    pub no_proxy_vm: bool,
}

impl VmProxyArgs<'_> {
    /// The options turned on explicitly that need a VM proxy.
    fn explicit_on(&self) -> Vec<&'static str> {
        [
            (self.proxy_udp, "--proxy-udp"),
            (self.proxy_dns, "--proxy-dns"),
            (self.strict_proxy, "--strict-proxy"),
        ]
        .into_iter()
        .filter(|(value, _)| *value == Some(true))
        .map(|(_, flag)| flag)
        .collect()
    }
}

/// Checks that need no environment: `--no-proxy-vm` and `vznat` conflicts,
/// the `--proxy` URL, and UDP or DNS over an explicit HTTP proxy.
/// Returns the normalized `--proxy` URL (curl form).
pub(crate) fn check_cli_proxy_args(
    args: &VmProxyArgs,
    network: &NetworkMode,
) -> Result<Option<String>, String> {
    let on = args.explicit_on();
    if args.no_proxy_vm
        && let Some(flag) = on.first()
    {
        return Err(format!("{flag} cannot be used with --no-proxy-vm"));
    }
    if *network == NetworkMode::VzNat
        && let Some(flag) = on.first()
    {
        return Err(format!(
            "{flag} cannot be used with --network vznat (vibe cannot enforce the proxy in vznat mode)"
        ));
    }
    let Some(url) = args.proxy else {
        return Ok(None);
    };
    let proxy = normalize_proxy_url(url).map_err(|err| format!("invalid --proxy: {err}"))?;
    if !args.no_proxy_vm {
        check_kind(args, &proxy, CLI_PROXY_SOURCE)?;
    }
    Ok(Some(proxy.curl_url))
}

fn check_kind(args: &VmProxyArgs, proxy: &NormalizedProxy, source: &str) -> Result<(), String> {
    if proxy.kind == ProxyKind::Socks5 {
        return Ok(());
    }
    for (value, flag) in [
        (args.proxy_udp, "--proxy-udp"),
        (args.proxy_dns, "--proxy-dns"),
    ] {
        if value == Some(true) {
            return Err(format!(
                "{flag} is only supported with socks5:// proxies; the proxy {} (from {source}) is an HTTP proxy",
                redact_proxy_url(&proxy.curl_url)
            ));
        }
    }
    Ok(())
}

/// The proxy the `vibe-usernet` helper uses (`nat` mode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VmProxy {
    pub url: String,
    pub kind: ProxyKind,
    pub source: &'static str,
    pub udp: bool,
    pub dns: bool,
    pub strict: bool,
    /// `None` means the built-in list.
    pub bypass: Option<BypassList>,
}

/// The proxy settings written into the guest (`vznat` mode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GuestProxy {
    /// Exported as `http_proxy`, `https_proxy`, and the upper-case forms.
    pub url: String,
    /// Written into the apt configuration; SOCKS5 uses `socks5h://`.
    pub apt_url: String,
    /// Exported as `no_proxy` and `NO_PROXY`.
    pub no_proxy: String,
}

impl GuestProxy {
    /// The summary line when provisioning saves the proxy into an image.
    pub(crate) fn image_note() -> String {
        format!(
            "  Image:  the proxy will be saved to {APT_PROXY_FILE} in the new image (with credentials, if any)"
        )
    }
}

/// The resolved VM proxy configuration, its summary, and warnings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VmProxyPlan {
    /// The helper's proxy (`nat` mode only).
    pub vm: Option<VmProxy>,
    /// The guest's proxy variables (`vznat` mode only).
    pub guest: Option<GuestProxy>,
    /// Summary lines, without warnings.
    pub summary: Vec<String>,
    /// Warnings, without the `warning: ` prefix.
    pub warnings: Vec<String>,
}

impl VmProxyPlan {
    /// The summary followed by `warning: ` lines. `saves_to_image` adds the
    /// line that says provisioning writes the proxy into the new image.
    pub(crate) fn describe(&self, saves_to_image: bool) -> String {
        let mut lines = self.summary.clone();
        if saves_to_image && self.guest.is_some() {
            lines.push(GuestProxy::image_note());
        }
        lines.extend(self.warnings.iter().map(|w| format!("warning: {w}")));
        lines.join("\n")
    }
}

fn mode_name(network: &NetworkMode) -> &'static str {
    match network {
        NetworkMode::Nat => "nat",
        NetworkMode::VzNat => "vznat",
    }
}

/// Resolves the VM proxy from the command line and the environment.
pub(crate) fn resolve_vm_proxy(
    args: &VmProxyArgs,
    network: &NetworkMode,
    env: &HostProxyEnv,
) -> Result<VmProxyPlan, String> {
    check_cli_proxy_args(args, network)?;
    let mode = mode_name(network);
    let no_proxy = |reason: &str| VmProxyPlan {
        vm: None,
        guest: None,
        summary: vec![format!("VM network ({mode}): no proxy{reason}")],
        warnings: Vec::new(),
    };

    if args.no_proxy_vm {
        return Ok(no_proxy(" (--no-proxy-vm)"));
    }

    let Some((proxy, source)) = proxy_from(args.proxy, env)? else {
        if let Some(flag) = args.explicit_on().first() {
            return Err(format!(
                "{flag} needs a proxy; set --proxy or one of {}",
                PROXY_ENV_VARS.join(", ")
            ));
        }
        let mut plan = no_proxy("");
        if args.proxy_bypass.is_some() {
            plan.warnings.push(
                "--proxy-bypass is ignored for VM traffic because no proxy is set".to_string(),
            );
        }
        return Ok(plan);
    };

    let bypass = bypass_from(args.proxy_bypass, env);
    let shown_url = redact_proxy_url(&proxy.curl_url);
    let bypass_summary = match &bypass {
        Some(list) if list.display().is_empty() => {
            format!("none (empty list from {})", list.source())
        }
        Some(list) => format!("{} (from {})", list.display(), list.source()),
        None => format!("built-in list ({})", builtin_bypass_text()),
    };
    let mut warnings: Vec<String> = bypass
        .iter()
        .flat_map(|list| list.vm_warnings().map(str::to_string))
        .collect();

    if *network == NetworkMode::VzNat {
        let no_proxy_text = bypass
            .as_ref()
            .map_or(builtin_bypass_text().to_string(), |list| {
                list.display().to_string()
            });
        let apt_url = match proxy.kind {
            ProxyKind::Socks5 => proxy.vm_url.replacen("socks5://", "socks5h://", 1),
            ProxyKind::Http => proxy.curl_url.clone(),
        };
        warnings.insert(
            0,
            "in vznat mode vibe cannot enforce the proxy; only guest programs that honor these variables use it. Make sure the proxy address is reachable from inside the guest.".to_string(),
        );
        return Ok(VmProxyPlan {
            vm: None,
            guest: Some(GuestProxy {
                url: proxy.curl_url.clone(),
                apt_url,
                no_proxy: no_proxy_text,
            }),
            summary: vec![
                format!("VM network ({mode}):"),
                format!(
                    "  Proxy:  {shown_url} (from {source}), exported to the guest as http_proxy/https_proxy"
                ),
                format!("  Bypass: {bypass_summary}, exported to the guest as no_proxy/NO_PROXY"),
            ],
            warnings,
        });
    }

    check_kind(args, &proxy, source)?;
    let socks = proxy.kind == ProxyKind::Socks5;
    let udp = args.proxy_udp.unwrap_or(socks);
    let dns = args.proxy_dns.unwrap_or(socks);
    let strict = args.strict_proxy.unwrap_or(false);

    let udp_line = match (udp, strict) {
        (true, _) => "proxied",
        (false, false) => "direct (not proxied)",
        (false, true) => "dropped, except destinations in the bypass list (direct)",
    };
    let dns_line = match (dns, strict) {
        (true, _) => "proxied (DNS-over-TCP); upstreams in the bypass list are queried directly",
        (false, false) => "direct (not proxied)",
        (false, true) => {
            "only upstreams in the bypass list, queried directly; REFUSED when there is none"
        }
    };
    let icmp_line = if strict {
        "only to destinations in the bypass list (direct); others dropped"
    } else {
        "direct (not proxied)"
    };

    let udp_reason = if socks {
        "--no-proxy-udp"
    } else {
        "HTTP proxies cannot carry UDP"
    };
    let dns_reason = if socks {
        "--no-proxy-dns"
    } else {
        "HTTP proxies cannot carry DNS"
    };
    if !udp && !strict {
        warnings.push(format!(
            "UDP from the VM is sent directly, not through the proxy ({udp_reason})"
        ));
    }
    if !udp && strict {
        warnings.push(format!(
            "strict proxy mode: UDP to destinations outside the bypass list is dropped ({udp_reason})"
        ));
    }
    if strict {
        warnings.push(
            "strict proxy mode: ICMP (ping) only reaches destinations in the bypass list"
                .to_string(),
        );
    }
    if !dns && !strict {
        warnings.push(format!(
            "gateway DNS queries are not sent through the proxy; they go directly to the --dns upstreams or, without --dns, to the host system resolver ({dns_reason})"
        ));
    }
    if !dns && strict {
        warnings.push(format!(
            "strict proxy mode: gateway DNS only uses upstreams in the bypass list, queried directly; when none is, every query is answered with REFUSED ({dns_reason})"
        ));
    }

    Ok(VmProxyPlan {
        vm: Some(VmProxy {
            url: proxy.vm_url,
            kind: proxy.kind,
            source,
            udp,
            dns,
            strict,
            bypass,
        }),
        guest: None,
        summary: vec![
            format!("VM network ({mode}):"),
            format!("  Proxy:  {shown_url} (from {source})"),
            format!("  Strict: {}", if strict { "on" } else { "off" }),
            "  TCP:    proxied".to_string(),
            format!("  UDP:    {udp_line}"),
            format!("  DNS:    {dns_line}"),
            format!("  ICMP:   {icmp_line}"),
            format!("  Bypass: {bypass_summary}; {GATEWAY_NOTE}"),
        ],
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(vars: &[(&str, &str)]) -> HostProxyEnv {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        HostProxyEnv::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn env_proxy_priority_and_empty_values() {
        let all = [
            ("HTTP_PROXY", "http://d:1"),
            ("http_proxy", "http://c:1"),
            ("HTTPS_PROXY", "http://b:1"),
            ("https_proxy", "http://a:1"),
        ];
        assert_eq!(
            env_of(&all).proxy,
            Some(("http://a:1".to_string(), "https_proxy"))
        );
        assert_eq!(
            env_of(&all[..3]).proxy,
            Some(("http://b:1".to_string(), "HTTPS_PROXY"))
        );
        assert_eq!(
            env_of(&all[..2]).proxy,
            Some(("http://c:1".to_string(), "http_proxy"))
        );
        assert_eq!(
            env_of(&all[..1]).proxy,
            Some(("http://d:1".to_string(), "HTTP_PROXY"))
        );
        let empty = env_of(&[("https_proxy", " "), ("http_proxy", "http://c:1")]);
        assert_eq!(empty.proxy, Some(("http://c:1".to_string(), "http_proxy")));
        assert_eq!(env_of(&[("ALL_PROXY", "socks5://x:1")]).proxy, None);

        let lists = env_of(&[("NO_PROXY", "b"), ("no_proxy", "a")]);
        assert_eq!(lists.no_proxy, Some(("a".to_string(), "no_proxy")));
        let upper = env_of(&[("NO_PROXY", "b"), ("no_proxy", "")]);
        assert_eq!(upper.no_proxy, Some(("b".to_string(), "NO_PROXY")));
    }

    #[test]
    fn proxy_urls_are_normalized() {
        let plain = normalize_proxy_url("192.168.1.88:3003").unwrap();
        assert_eq!(plain.vm_url, "http://192.168.1.88:3003");
        assert_eq!(plain.curl_url, "http://192.168.1.88:3003");
        assert_eq!(plain.kind, ProxyKind::Http);

        let socks_h = normalize_proxy_url("SOCKS5H://u:p@host:1080").unwrap();
        assert_eq!(socks_h.vm_url, "socks5://u:p@host:1080");
        assert_eq!(socks_h.curl_url, "socks5h://u:p@host:1080");
        assert_eq!(socks_h.kind, ProxyKind::Socks5);

        assert_eq!(
            normalize_proxy_url("https://p:8443").unwrap().kind,
            ProxyKind::Http
        );

        for bad in [
            "socks4://h:1",
            "socks://h:1",
            "ftp://h:1",
            "",
            "http://",
            "http://u:p@",
        ] {
            assert!(normalize_proxy_url(bad).is_err(), "{bad}");
        }
        let err = normalize_proxy_url("socks4://user:secret@h:1").unwrap_err();
        assert!(!err.contains("secret"), "{err}");
    }

    #[test]
    fn credentials_are_redacted() {
        assert_eq!(
            redact_proxy_url("http://user:pass@host:8080/x"),
            "http://***@host:8080/x"
        );
        assert_eq!(redact_proxy_url("u:p@host:1"), "***@host:1");
        assert_eq!(redact_proxy_url("socks5://host:1"), "socks5://host:1");
        assert_eq!(redact_proxy_url("http://host/a@b"), "http://host/a@b");
    }

    #[test]
    fn bypass_list_items() {
        let list = parse_bypass_list(
            " 127.0.0.1, LOCALHOST,192.168.*,10.*, 172.16.0.0/12, ::1, fe80::/10, \
             example.com, .example.org, *.example.net, 10.1.2.3:8080, [::1]:8080, *, \
             10.0.0.0/99, foo bar,",
            "--proxy-bypass",
        );
        assert_eq!(
            list.usernet_arg(),
            "127.0.0.1/32,127.0.0.0/8,::1/128,192.168.0.0/16,10.0.0.0/8,172.16.0.0/12,\
fe80::/10,10.1.2.3/32:8080,[::1/128]:8080,*"
        );
        assert_eq!(
            list.curl_noproxy(),
            "127.0.0.1,localhost,192.168.0.0/16,10.0.0.0/8,172.16.0.0/12,::1,fe80::/10,\
example.com,.example.org,.example.net,*"
        );
        let vm: Vec<_> = list.vm_warnings().collect();
        let downloads: Vec<_> = list.download_warnings().collect();
        assert!(vm.iter().any(|w| w.contains("\"example.com\" is a domain")
            && w.contains("only applies to host downloads")));
        assert!(
            vm.iter()
                .any(|w| w.contains("\"*.example.net\" is a domain"))
        );
        assert!(!downloads.iter().any(|w| w.contains("is a domain")));
        assert!(
            downloads
                .iter()
                .any(|w| w.contains("\"10.1.2.3:8080\" has a port"))
        );
        assert!(
            downloads
                .iter()
                .any(|w| w.contains("\"[::1]:8080\" has a port"))
        );
        assert!(!vm.iter().any(|w| w.contains("has a port")));
        for warnings in [&vm, &downloads] {
            assert!(warnings.iter().any(|w| w.contains("\"*\" bypasses")));
            assert!(
                warnings
                    .iter()
                    .any(|w| w.contains("\"10.0.0.0/99\"") && w.contains("--proxy-bypass"))
            );
            assert!(warnings.iter().any(|w| w.contains("\"foo bar\"")));
        }
        assert_eq!(list.display(), list.raw);
        assert_eq!(list.source(), "--proxy-bypass");
    }

    #[test]
    fn bypass_list_localhost_needs_no_warning() {
        let list = parse_bypass_list("localhost,localhost.localdomain", "no_proxy");
        assert_eq!(list.usernet_arg(), "127.0.0.0/8,::1/128");
        assert_eq!(list.vm_warnings().count(), 0);
        assert_eq!(list.download_warnings().count(), 0);
        assert_eq!(parse_bypass_list("", "no_proxy").usernet_arg(), "");
    }

    #[test]
    fn download_proxy_sources() {
        let env = env_of(&[
            ("https_proxy", "http://env:1"),
            ("no_proxy", "127.0.0.1,localhost,192.168.*"),
        ]);
        let from_env = DownloadProxy::resolve(None, None, &env).unwrap();
        assert_eq!(
            from_env.curl_args(),
            [
                "--proxy",
                "http://env:1",
                "--noproxy",
                "127.0.0.1,localhost,192.168.0.0/16"
            ]
        );
        assert_eq!(
            from_env.describe(),
            "Host downloads proxy: http://env:1 (from https_proxy); bypass 127.0.0.1,localhost,192.168.0.0/16 (from no_proxy)"
        );

        let cli =
            DownloadProxy::resolve(Some("socks5h://u:p@cli:2"), Some("10.*,a.example:80"), &env)
                .unwrap();
        assert_eq!(
            cli.curl_args(),
            ["--proxy", "socks5h://u:p@cli:2", "--noproxy", "10.0.0.0/8"]
        );
        let shown = cli.describe();
        assert!(shown.starts_with("Host downloads proxy: socks5h://***@cli:2 (from --proxy)"));
        assert!(shown.contains("\nwarning: ignoring --proxy-bypass item \"a.example:80\""));

        let none = DownloadProxy::resolve(None, None, &HostProxyEnv::default()).unwrap();
        assert_eq!(none.curl_args(), ["--proxy", ""]);
        assert_eq!(none.describe(), "Host downloads proxy: none");

        assert!(
            DownloadProxy::resolve(None, None, &env_of(&[("http_proxy", "socks4://x:1")]))
                .unwrap_err()
                .contains("http_proxy")
        );
    }

    fn args() -> VmProxyArgs<'static> {
        VmProxyArgs::default()
    }

    fn nat(args: &VmProxyArgs, env: &HostProxyEnv) -> Result<VmProxyPlan, String> {
        resolve_vm_proxy(args, &NetworkMode::Nat, env)
    }

    const SOCKS: &str = "socks5://192.168.1.88:2083";

    #[test]
    fn vm_proxy_sources() {
        let env = env_of(&[("https_proxy", "http://env:1"), ("no_proxy", "192.168.*")]);
        let plan = nat(&args(), &env).unwrap();
        let vm = plan.vm.unwrap();
        assert_eq!(
            (vm.url.as_str(), vm.source),
            ("http://env:1", "https_proxy")
        );
        assert_eq!(vm.bypass.unwrap().source(), "no_proxy");

        let cli = VmProxyArgs {
            proxy: Some(SOCKS),
            proxy_bypass: Some("10.*"),
            ..args()
        };
        let vm = nat(&cli, &env).unwrap().vm.unwrap();
        assert_eq!((vm.url.as_str(), vm.source), (SOCKS, "--proxy"));
        assert_eq!(vm.bypass.unwrap().usernet_arg(), "10.0.0.0/8");

        let upper = env_of(&[("http_proxy", SOCKS), ("NO_PROXY", "10.*")]);
        assert_eq!(
            nat(&args(), &upper)
                .unwrap()
                .vm
                .unwrap()
                .bypass
                .unwrap()
                .source(),
            "NO_PROXY"
        );
        let builtin = nat(&args(), &env_of(&[("http_proxy", SOCKS)])).unwrap();
        assert_eq!(builtin.vm.unwrap().bypass, None);
        assert!(builtin.summary.last().unwrap().contains("built-in list"));
    }

    #[test]
    fn no_proxy_vm_disables_the_vm_proxy() {
        let env = env_of(&[("https_proxy", "http://env:1")]);
        let off = VmProxyArgs {
            proxy: Some(SOCKS),
            proxy_bypass: Some("10.*"),
            proxy_udp: Some(false),
            strict_proxy: Some(false),
            no_proxy_vm: true,
            ..args()
        };
        let plan = nat(&off, &env).unwrap();
        assert_eq!(plan.vm, None);
        assert_eq!(
            plan.describe(false),
            "VM network (nat): no proxy (--no-proxy-vm)"
        );

        for on in [
            VmProxyArgs {
                proxy_udp: Some(true),
                no_proxy_vm: true,
                ..args()
            },
            VmProxyArgs {
                proxy_dns: Some(true),
                no_proxy_vm: true,
                ..args()
            },
            VmProxyArgs {
                strict_proxy: Some(true),
                no_proxy_vm: true,
                ..args()
            },
        ] {
            assert!(nat(&on, &env).unwrap_err().contains("--no-proxy-vm"));
        }
    }

    #[test]
    fn udp_dns_strict_defaults_and_errors() {
        let none = HostProxyEnv::default();
        let socks = VmProxyArgs {
            proxy: Some(SOCKS),
            ..args()
        };
        let vm = nat(&socks, &none).unwrap().vm.unwrap();
        assert!(vm.udp && vm.dns && !vm.strict);

        let http = VmProxyArgs {
            proxy: Some("http://h:1"),
            ..args()
        };
        let vm = nat(&http, &none).unwrap().vm.unwrap();
        assert!(!vm.udp && !vm.dns && !vm.strict);

        let explicit = VmProxyArgs {
            proxy: Some(SOCKS),
            proxy_udp: Some(false),
            proxy_dns: Some(false),
            strict_proxy: Some(true),
            ..args()
        };
        let vm = nat(&explicit, &none).unwrap().vm.unwrap();
        assert!(!vm.udp && !vm.dns && vm.strict);

        let http_udp = VmProxyArgs {
            proxy_udp: Some(true),
            ..http.clone()
        };
        assert!(
            nat(&http_udp, &none)
                .unwrap_err()
                .contains("--proxy-udp is only supported with socks5:// proxies")
        );
        // A proxy from the environment is checked the same way.
        let env_http = env_of(&[("https_proxy", "http://env:1")]);
        let dns_on = VmProxyArgs {
            proxy_dns: Some(true),
            ..args()
        };
        let err = nat(&dns_on, &env_http).unwrap_err();
        assert!(
            err.contains("--proxy-dns") && err.contains("https_proxy"),
            "{err}"
        );

        // Strict mode works with a proxy from the environment.
        let strict = VmProxyArgs {
            strict_proxy: Some(true),
            ..args()
        };
        assert!(nat(&strict, &env_http).unwrap().vm.unwrap().strict);
    }

    #[test]
    fn no_proxy_at_all() {
        let none = HostProxyEnv::default();
        for (on, flag) in [
            (
                VmProxyArgs {
                    proxy_udp: Some(true),
                    ..args()
                },
                "--proxy-udp",
            ),
            (
                VmProxyArgs {
                    proxy_dns: Some(true),
                    ..args()
                },
                "--proxy-dns",
            ),
            (
                VmProxyArgs {
                    strict_proxy: Some(true),
                    ..args()
                },
                "--strict-proxy",
            ),
        ] {
            assert!(nat(&on, &none).unwrap_err().starts_with(flag));
        }
        let bypass = VmProxyArgs {
            proxy_bypass: Some("10.*"),
            ..args()
        };
        let plan = nat(&bypass, &none).unwrap();
        assert_eq!(plan.vm, None);
        assert_eq!(
            plan.describe(false),
            "VM network (nat): no proxy\nwarning: --proxy-bypass is ignored for VM traffic because no proxy is set"
        );
        let off = VmProxyArgs {
            proxy_udp: Some(false),
            strict_proxy: Some(false),
            ..args()
        };
        assert_eq!(nat(&off, &none).unwrap().vm, None);
    }

    #[test]
    fn warnings_for_each_combination() {
        let none = HostProxyEnv::default();
        let has = |plan: &VmProxyPlan, text: &str| plan.warnings.iter().any(|w| w.contains(text));

        let socks = nat(
            &VmProxyArgs {
                proxy: Some(SOCKS),
                ..args()
            },
            &none,
        )
        .unwrap();
        assert!(socks.warnings.is_empty(), "{:?}", socks.warnings);

        let http = nat(
            &VmProxyArgs {
                proxy: Some("http://h:1"),
                ..args()
            },
            &none,
        )
        .unwrap();
        assert!(has(&http, "UDP from the VM is sent directly"));
        assert!(has(&http, "HTTP proxies cannot carry UDP"));
        assert!(has(
            &http,
            "gateway DNS queries are not sent through the proxy"
        ));
        assert!(!has(&http, "strict"));

        let strict = nat(
            &VmProxyArgs {
                proxy: Some(SOCKS),
                proxy_udp: Some(false),
                proxy_dns: Some(false),
                strict_proxy: Some(true),
                ..args()
            },
            &none,
        )
        .unwrap();
        assert!(has(
            &strict,
            "UDP to destinations outside the bypass list is dropped"
        ));
        assert!(has(
            &strict,
            "ICMP (ping) only reaches destinations in the bypass list"
        ));
        assert!(has(&strict, "REFUSED"));
        assert!(!has(&strict, "sent directly"));

        let domain = nat(
            &VmProxyArgs {
                proxy: Some(SOCKS),
                proxy_bypass: Some("example.com"),
                ..args()
            },
            &none,
        )
        .unwrap();
        assert!(has(&domain, "\"example.com\" is a domain"));
    }

    #[test]
    fn nat_summary() {
        let env = env_of(&[("no_proxy", "127.0.0.1,localhost,192.168.*")]);
        let plan = nat(
            &VmProxyArgs {
                proxy: Some("socks5://u:p@192.168.1.88:2083"),
                ..args()
            },
            &env,
        )
        .unwrap();
        assert_eq!(
            plan.describe(true),
            "VM network (nat):
  Proxy:  socks5://***@192.168.1.88:2083 (from --proxy)
  Strict: off
  TCP:    proxied
  UDP:    proxied
  DNS:    proxied (DNS-over-TCP); upstreams in the bypass list are queried directly
  ICMP:   direct (not proxied)
  Bypass: 127.0.0.1,localhost,192.168.* (from no_proxy); gateway 192.168.5.2 -> host is always direct"
        );
    }

    #[test]
    fn vznat_exports_the_proxy() {
        let env = env_of(&[
            ("https_proxy", "http://127.0.0.1:8132"),
            ("no_proxy", "127.0.0.1,192.168.*"),
        ]);
        let plan = resolve_vm_proxy(&args(), &NetworkMode::VzNat, &env).unwrap();
        assert_eq!(plan.vm, None);
        assert_eq!(
            plan.guest,
            Some(GuestProxy {
                url: "http://127.0.0.1:8132".to_string(),
                apt_url: "http://127.0.0.1:8132".to_string(),
                no_proxy: "127.0.0.1,192.168.*".to_string(),
            })
        );
        assert_eq!(
            plan.describe(true),
            "VM network (vznat):
  Proxy:  http://127.0.0.1:8132 (from https_proxy), exported to the guest as http_proxy/https_proxy
  Bypass: 127.0.0.1,192.168.* (from no_proxy), exported to the guest as no_proxy/NO_PROXY
  Image:  the proxy will be saved to /etc/apt/apt.conf.d/99timeout in the new image (with credentials, if any)
warning: in vznat mode vibe cannot enforce the proxy; only guest programs that honor these variables use it. Make sure the proxy address is reachable from inside the guest."
        );

        let socks = resolve_vm_proxy(
            &VmProxyArgs {
                proxy: Some("socks5h://h:1"),
                ..args()
            },
            &NetworkMode::VzNat,
            &HostProxyEnv::default(),
        )
        .unwrap()
        .guest
        .unwrap();
        assert_eq!(socks.url, "socks5h://h:1");
        assert_eq!(socks.apt_url, "socks5h://h:1");
        assert_eq!(socks.no_proxy, builtin_bypass_text());

        let plain_socks = resolve_vm_proxy(
            &VmProxyArgs {
                proxy: Some("socks5://h:1"),
                ..args()
            },
            &NetworkMode::VzNat,
            &HostProxyEnv::default(),
        )
        .unwrap()
        .guest
        .unwrap();
        assert_eq!(plain_socks.apt_url, "socks5h://h:1");

        for on in [
            VmProxyArgs {
                proxy_udp: Some(true),
                ..args()
            },
            VmProxyArgs {
                proxy_dns: Some(true),
                ..args()
            },
            VmProxyArgs {
                strict_proxy: Some(true),
                ..args()
            },
        ] {
            assert!(
                resolve_vm_proxy(&on, &NetworkMode::VzNat, &env)
                    .unwrap_err()
                    .contains("--network vznat")
            );
        }
    }

    #[test]
    fn builtin_text_matches_the_canonical_list() {
        // The text form must stay equivalent to the Go built-in list.
        let list = parse_bypass_list(builtin_bypass_text(), "built-in");
        assert_eq!(list.vm_warnings().count(), 0);
        assert!(list.usernet_arg().contains("192.168.0.0/16"));
    }
}
