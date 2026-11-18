//! Vibe's default NAT uses a bundled gVisor/Lima-style user-mode network stack.
//! The helper receives a connected VZ datagram fd and exits when Vibe drops the liveness fd.

use std::{
    fs,
    io::{self, BufRead, BufReader, Read},
    os::{
        fd::{AsRawFd, OwnedFd, RawFd},
        unix::{fs::PermissionsExt, net::UnixDatagram, process::CommandExt},
    },
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use crate::proxy::VmProxy;

const USERNET_HELPER: &[u8] = include_bytes!(env!("BUNDLED_USERNET_HELPER_PATH"));

const USERNET_HELPER_FD: RawFd = 3;
const USERNET_PARENT_LIVENESS_FD: RawFd = 4;
const USERNET_SENDBUF_BYTES: libc::c_int = 1024 * 1024;
const USERNET_RECVBUF_BYTES: libc::c_int = 1024 * 1024;
const USERNET_READY_MESSAGE: &str = "ready";
/// Prefix of the helper's line that describes the gateway's DNS upstreams:
/// `dns-upstreams <mode> <description>`.
const USERNET_DNS_UPSTREAMS_PREFIX: &str = "dns-upstreams ";
const USERNET_READY_TIMEOUT: Duration = Duration::from_secs(3);

pub const USERNET_MAC_ADDRESS: &str = "5a:94:ef:e4:0c:df";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkMode {
    Nat,
    VzNat,
}

pub enum PreparedNetworkBackend {
    VzNat,
    Usernet {
        vm_socket_fd: Option<OwnedFd>,
        _liveness: OwnedFd,
        _helper: UsernetHelperProcess,
    },
}

impl NetworkMode {
    pub fn parse(value: &str) -> Result<Self, Box<dyn std::error::Error>> {
        match value {
            "nat" => Ok(Self::Nat),
            "vznat" => Ok(Self::VzNat),
            _ => Err(
                format!("Unsupported --network value '{value}'; expected 'nat' or 'vznat'").into(),
            ),
        }
    }

    pub fn prepare(
        &self,
        usernet_helper_path: &Path,
        log_path: Option<&Path>,
        publish: &[String],
        proxy: Option<&VmProxy>,
        dns: &[String],
    ) -> Result<PreparedNetworkBackend, Box<dyn std::error::Error>> {
        match self {
            NetworkMode::VzNat => Ok(PreparedNetworkBackend::VzNat),
            NetworkMode::Nat => {
                ensure_usernet_helper_extracted(usernet_helper_path);

                let mut command = Command::new(usernet_helper_path);
                command
                    .args(usernet_args(publish, proxy, dns))
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());

                let (vm_socket_fd, helper_socket_fd) = create_datagram_pair();
                let helper_raw_fd = helper_socket_fd.as_raw_fd();

                let (helper_liveness_read_fd, helper_liveness_write_fd) = crate::create_pipe();
                let helper_liveness_raw_fd = helper_liveness_read_fd.as_raw_fd();

                unsafe {
                    command.pre_exec(move || {
                        let fds = [
                            (helper_raw_fd, USERNET_HELPER_FD),
                            (helper_liveness_raw_fd, USERNET_PARENT_LIVENESS_FD),
                        ];

                        for (src_fd, dst_fd) in fds {
                            if libc::dup2(src_fd, dst_fd) == -1 {
                                return Err(io::Error::last_os_error());
                            }
                            if src_fd != dst_fd {
                                libc::close(src_fd);
                            }
                        }
                        Ok(())
                    });
                }

                let mut helper = UsernetHelperProcess {
                    child: command.spawn()?,
                };
                helper.wait_until_ready(log_path)?;

                Ok(PreparedNetworkBackend::Usernet {
                    vm_socket_fd: Some(vm_socket_fd),
                    _liveness: helper_liveness_write_fd,
                    _helper: helper,
                })
            }
        }
    }
}

/// The `vibe-usernet` command line. Proxy settings are always passed
/// explicitly, so the helper's own defaults never apply.
fn usernet_args(publish: &[String], proxy: Option<&VmProxy>, dns: &[String]) -> Vec<String> {
    let mut args = vec![
        "--fd".to_string(),
        USERNET_HELPER_FD.to_string(),
        "--parent-liveness-fd".to_string(),
        USERNET_PARENT_LIVENESS_FD.to_string(),
        "--mac".to_string(),
        USERNET_MAC_ADDRESS.to_string(),
    ];
    for spec in publish {
        args.push("-p".to_string());
        args.push(spec.clone());
    }
    if let Some(proxy) = proxy {
        args.push("--proxy".to_string());
        args.push(proxy.url.clone());
        args.push(format!("--proxy-udp={}", proxy.udp));
        args.push(format!("--proxy-dns={}", proxy.dns));
        args.push(format!("--strict-proxy={}", proxy.strict));
        if let Some(bypass) = &proxy.bypass {
            args.push("--proxy-bypass".to_string());
            args.push(bypass.usernet_arg());
        }
    }
    for addr in dns {
        args.push("--dns".to_string());
        args.push(addr.clone());
    }
    args
}

/// Turns the helper's `dns-upstreams <mode> <description>` line into the
/// lines printed for the user.
fn describe_dns_upstreams(line: &str) -> Option<String> {
    let rest = line.strip_prefix(USERNET_DNS_UPSTREAMS_PREFIX)?;
    let (mode, description) = rest.split_once(' ').unwrap_or((rest, ""));
    let mut out = format!("VM network DNS: {description}");
    if mode == "refused" {
        out.push_str(
            "\nwarning: strict proxy mode leaves the gateway DNS without an upstream; every query is answered with REFUSED. Add an upstream in the bypass list with --dns, or allow DNS through the proxy.",
        );
    }
    Some(out)
}

fn configure_usernet_socket(fd: RawFd) {
    for (name, opt, value) in [
        ("SO_SNDBUF", libc::SO_SNDBUF, USERNET_SENDBUF_BYTES),
        ("SO_RCVBUF", libc::SO_RCVBUF, USERNET_RECVBUF_BYTES),
    ] {
        let status = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                (&raw const value).cast::<libc::c_void>(),
                std::mem::size_of_val(&value) as libc::socklen_t,
            )
        };
        if status != 0 {
            eprintln!(
                "Warning: failed to set {name} on usernet socket: {}",
                io::Error::last_os_error()
            );
        }
    }
}

fn create_datagram_pair() -> (OwnedFd, OwnedFd) {
    let (left, right) = UnixDatagram::pair().expect("Failed to create datagram pair");
    configure_usernet_socket(left.as_raw_fd());
    configure_usernet_socket(right.as_raw_fd());
    (left.into(), right.into())
}

fn ensure_usernet_helper_extracted(path: &Path) {
    if fs::read(path).ok().as_deref() == Some(USERNET_HELPER) {
        return;
    }
    let temp_path = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temp_path, USERNET_HELPER).unwrap();
    fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&temp_path, path).unwrap();
}

pub struct UsernetHelperProcess {
    child: Child,
}

impl UsernetHelperProcess {
    fn wait_until_ready(
        &mut self,
        log_path: Option<&Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let stdout = self
            .child
            .stdout
            .take()
            .ok_or("vibe-usernet stdout was not captured")?;
        let mut reader = BufReader::new(stdout);
        let deadline = Instant::now() + USERNET_READY_TIMEOUT;

        loop {
            if reader.buffer().is_empty() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let mut pollfd = libc::pollfd {
                    fd: reader.get_ref().as_raw_fd(),
                    events: libc::POLLIN | libc::POLLHUP,
                    revents: 0,
                };
                let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
                let poll_result = unsafe { libc::poll(&raw mut pollfd, 1, timeout_ms) };
                if poll_result < 0 {
                    return Err(io::Error::last_os_error().into());
                }
                if poll_result == 0 {
                    return Err("Timed out waiting for vibe-usernet to become ready.".into());
                }
            }

            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                let status = self.child.wait()?;
                let stderr = self.read_stderr();

                return Err(format!(
                    "vibe-usernet exited before becoming ready ({}){}",
                    match status.code() {
                        Some(code) => format!("exit code {code}"),
                        None => "terminated by signal".to_string(),
                    },
                    stderr
                        .filter(|stderr| !stderr.is_empty())
                        .map(|stderr| format!(": {stderr}"))
                        .unwrap_or_default(),
                )
                .into());
            }

            let line = line.trim_end_matches(&['\r', '\n'][..]);
            if line == USERNET_READY_MESSAGE {
                break;
            }
            match describe_dns_upstreams(line) {
                Some(description) => println!("{description}"),
                None => {
                    return Err(format!("vibe-usernet sent unexpected ready line: {line:?}").into());
                }
            }
        }

        // Drain vibe-usernet's stdout/stderr so OS pipe buffers don't fill up and block the helper.
        // Log stderr to a path, if one was provided.
        let mut stderr_sink = log_path
            .and_then(|dir| {
                fs::create_dir_all(dir).ok()?;
                fs::File::create(dir.join("vibe-usernet.log")).ok()
            })
            .map(|file| Box::new(file) as Box<dyn io::Write + Send>)
            .unwrap_or_else(|| Box::new(io::sink()));

        std::thread::spawn(move || {
            let _ = io::copy(&mut reader, &mut io::sink());
        });
        if let Some(mut stderr) = self.child.stderr.take() {
            std::thread::spawn(move || {
                let _ = io::copy(&mut stderr, &mut stderr_sink);
            });
        }

        Ok(())
    }

    fn read_stderr(&mut self) -> Option<String> {
        let mut stderr = String::new();
        self.child.stderr.take()?.read_to_string(&mut stderr).ok()?;
        Some(stderr.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::{ProxyKind, parse_bypass_list};

    fn base_args() -> Vec<String> {
        usernet_args(&[], None, &[])
    }

    #[test]
    fn usernet_args_without_proxy() {
        let args = usernet_args(&["8080:80".to_string()], None, &["1.1.1.1".to_string()]);
        let mut want = base_args();
        want.extend(["-p", "8080:80", "--dns", "1.1.1.1"].map(String::from));
        assert_eq!(args, want);
        assert!(!args.iter().any(|arg| arg.starts_with("--proxy")));
    }

    #[test]
    fn usernet_args_pass_every_proxy_setting() {
        let mut proxy = VmProxy {
            url: "socks5://192.168.1.88:2083".to_string(),
            kind: ProxyKind::Socks5,
            source: "--proxy",
            udp: true,
            dns: false,
            strict: true,
            bypass: None,
        };
        let mut want = base_args();
        want.extend(
            [
                "--proxy",
                "socks5://192.168.1.88:2083",
                "--proxy-udp=true",
                "--proxy-dns=false",
                "--strict-proxy=true",
            ]
            .map(String::from),
        );
        assert_eq!(usernet_args(&[], Some(&proxy), &[]), want);

        proxy.bypass = Some(parse_bypass_list("192.168.*,example.com", "no_proxy"));
        let args = usernet_args(&[], Some(&proxy), &[]);
        assert_eq!(args[args.len() - 2..], ["--proxy-bypass", "192.168.0.0/16"]);

        proxy.bypass = Some(parse_bypass_list("", "--proxy-bypass"));
        let args = usernet_args(&[], Some(&proxy), &[]);
        assert_eq!(args[args.len() - 2..], ["--proxy-bypass", ""]);
    }

    #[test]
    fn dns_upstreams_line() {
        assert_eq!(
            describe_dns_upstreams(
                "dns-upstreams proxied upstreams from --dns: 8.8.8.8:53 (via proxy)"
            )
            .unwrap(),
            "VM network DNS: upstreams from --dns: 8.8.8.8:53 (via proxy)"
        );
        let refused = describe_dns_upstreams("dns-upstreams refused no upstream").unwrap();
        assert!(refused.contains("\nwarning: ") && refused.contains("REFUSED"));
        assert_eq!(describe_dns_upstreams("ready"), None);
    }
}
