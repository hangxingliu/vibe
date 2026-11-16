//! Command line parsing.

use std::{
    borrow::Cow, collections::HashMap, env, ffi::OsString, fs, path::PathBuf, time::Duration,
};

use lexopt::prelude::*;

use crate::networking::NetworkMode;
use crate::session::{DEFAULT_EXPECT_TIMEOUT, LoginAction, LoginAction::*, RootPassword};
use crate::shares::GitMode;
use crate::vm_user::VmUser;
use crate::{
    BUILTIN_PROVISION_SCRIPTS, BYTES_PER_MB, DEFAULT_CPU_COUNT, DEFAULT_IMAGE_NAME,
    DEFAULT_PROVISION_RAM_BYTES, DEFAULT_RAM_BYTES, ProvisionScript, help,
    images::assert_valid_image_name, proxy,
};

#[cfg(test)]
mod tests;

pub(crate) struct CliArgs {
    pub(crate) command: CliCommand,
    pub(crate) version: bool,
    pub(crate) help: bool,
    /// Default true. Run and provision.
    pub(crate) mount_sys_cache: bool,
    /// Default true. Run and provision.
    pub(crate) mount_mise: bool,
    /// Default true. Run only.
    pub(crate) mount_cwd: bool,
    /// Default false. Run only.
    pub(crate) mount_tmux_conf: bool,
    /// Default false. Run only.
    pub(crate) mount_agents_conf: bool,
    /// Default false. Run only; provisioning always writes `/root/.profile`.
    pub(crate) write_bashrc: bool,
    pub(crate) env: HashMap<String, String>,
    pub(crate) mounts: Vec<String>,
    pub(crate) login_actions: Vec<LoginAction>,
    pub(crate) network_mode: NetworkMode,
    pub(crate) cpu_count: usize,
    pub(crate) ram_bytes: u64,
    /// `--proxy`, normalized (scheme added; `socks5h://` kept for curl).
    pub(crate) proxy: Option<String>,
    pub(crate) proxy_bypass: Option<String>,
    /// `Some` when `--proxy-udp` or `--no-proxy-udp` was given (last wins).
    pub(crate) proxy_udp: Option<bool>,
    pub(crate) proxy_dns: Option<bool>,
    pub(crate) strict_proxy: Option<bool>,
    pub(crate) no_proxy_vm: bool,
    pub(crate) dns: Vec<String>,
    pub(crate) ssh_key: Option<PathBuf>,
    pub(crate) publish: Vec<String>,
    pub(crate) git_mode: GitMode,
    /// The account the guest paths and checks are prepared for. Run checks
    /// it exists; a provision creates it. The session's shell is still root.
    pub(crate) vm_user: VmUser,
    pub(crate) root_password: Option<RootPassword>,
}

pub(crate) enum CliCommand {
    Run {
        disk: Option<PathBuf>,
        image: String,
    },
    Provision {
        base: Option<String>,
        image: String,
        replace: bool,
        repl: bool,
        scripts: Vec<ProvisionScript>,
        cpu_count: usize,
        ram_bytes: u64,
    },
}

pub(crate) fn cannot_use_with_provision(flag: &str) -> Box<dyn std::error::Error> {
    format!("{flag} cannot be used with provision").into()
}

pub(crate) fn only_valid_with_provision(flag: &str) -> Box<dyn std::error::Error> {
    format!("{flag} is only valid with provision").into()
}

pub(crate) fn load_provision_script(
    name: String,
) -> Result<ProvisionScript, Box<dyn std::error::Error>> {
    if let Some(builtin) = name.strip_prefix('@') {
        return BUILTIN_PROVISION_SCRIPTS
            .iter()
            .find(|script| script.name == builtin)
            .cloned()
            .ok_or_else(|| -> Box<dyn std::error::Error> {
                format!(
                    "Unknown built-in provisioning script: @{builtin}\n\n{}",
                    help::provisioning_scripts_banner()
                )
                .into()
            });
    }

    let content = fs::read_to_string(&name)
        .map_err(|err| format!("Failed to read provisioning script {name}: {err}"))?;
    Ok(ProvisionScript {
        content: Cow::Owned(content),
        name: Cow::Owned(name),
        description: Cow::Borrowed(""),
    })
}

pub(crate) fn parse_cli() -> Result<CliArgs, Box<dyn std::error::Error>> {
    parse_cli_from(env::args_os().skip(1), read_root_password_env()?.as_deref())
}

/// `Ok(None)` when `VIBE_ROOT_PASSWORD` is unset. Non-UTF-8 is an error so a
/// password the parser cannot check is not silently ignored.
pub(crate) fn read_root_password_env() -> Result<Option<String>, Box<dyn std::error::Error>> {
    match env::var("VIBE_ROOT_PASSWORD") {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err("VIBE_ROOT_PASSWORD is not valid UTF-8".into()),
    }
}

pub(crate) fn parse_cli_from<I>(
    args: I,
    root_password_env: Option<&str>,
) -> Result<CliArgs, Box<dyn std::error::Error>>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    fn os_to_string(value: OsString, flag: &str) -> Result<String, Box<dyn std::error::Error>> {
        value
            .into_string()
            .map_err(|_| format!("{flag} expects valid UTF-8").into())
    }

    fn parse_ram_size(
        parser: &mut lexopt::Parser,
    ) -> Result<u64, Box<dyn std::error::Error + 'static>> {
        let value: u64 = os_to_string(parser.value()?, "--ram")?.parse()?;
        if value == 0 {
            return Err("--ram must be >= 1".into());
        }
        Ok(value * BYTES_PER_MB)
    }

    fn parse_cpu_count(
        parser: &mut lexopt::Parser,
    ) -> Result<usize, Box<dyn std::error::Error + 'static>> {
        let value: usize = os_to_string(parser.value()?, "--cpus")?.parse()?;
        if value == 0 {
            return Err("--cpus must be >= 1".into());
        }
        Ok(value)
    }

    // Run-only flags are parsed in either position so a value is never left
    // for a later flag to consume. They are rejected once `provision` is selected.
    fn reject_run_only(
        parsing_provision: bool,
        run_only: &mut Option<&'static str>,
        flag: &'static str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if parsing_provision {
            return Err(cannot_use_with_provision(flag));
        }
        if run_only.is_none() {
            *run_only = Some(flag);
        }
        Ok(())
    }

    // A run-only `--NAME` / `--no-NAME` pair; the last form given wins.
    // Returns true for `--NAME` (`on`) and false for `--no-NAME` (`off`).
    fn run_only_switch(
        parsing_provision: bool,
        run_only: &mut Option<&'static str>,
        flag: &str,
        on: &'static str,
        off: &'static str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let enabled = flag == on.trim_start_matches('-');
        reject_run_only(parsing_provision, run_only, if enabled { on } else { off })?;
        Ok(enabled)
    }

    let mut parser = lexopt::Parser::from_args(args);
    let mut parsing_provision = false;
    let mut run_only: Option<&'static str> = None;
    let mut disk = None;
    let mut base = None;
    let mut replace = false;
    let mut repl = false;
    let mut scripts = Vec::new();
    let mut image = DEFAULT_IMAGE_NAME.to_string();
    let mut image_seen = false;
    let mut version = false;
    let mut help = false;
    let mut mount_sys_cache = true;
    let mut mount_mise = true;
    let mut mount_cwd = true;
    let mut mount_tmux_conf = false;
    let mut mount_agents_conf = false;
    let mut write_bashrc = false;
    let mut env_vars = HashMap::new();
    let mut mounts = Vec::new();
    let mut login_actions = Vec::new();
    let mut network_mode = NetworkMode::Nat;
    let mut cpu_count = DEFAULT_CPU_COUNT;
    let mut ram_bytes = DEFAULT_RAM_BYTES;
    // Running and provisioning use different RAM defaults. An explicit `--ram`
    // applies to whichever command is selected; an omitted one keeps that default.
    let mut ram_explicit = false;
    let mut proxy = None;
    let mut proxy_bypass = None;
    let mut proxy_udp = None;
    let mut proxy_dns = None;
    let mut strict_proxy = None;
    let mut no_proxy_vm = false;
    let mut dns: Vec<String> = Vec::new();
    let mut ssh_key = None;
    let mut publish = Vec::new();
    let mut git_mode = GitMode::Ro;
    let mut vm_user = VmUser::root();
    let mut root_password_flag: Option<String> = None;

    // Options fall into three classes:
    // - Shared options (image, resources, user, proxy) work before or after
    //   `provision`.
    // - Run-only options are parsed in any position so their values are
    //   consumed. Before `provision` the command is not known yet, so the first
    //   one is only remembered in `run_only`. The error is raised when the
    //   `provision` word arrives; after it, the error is immediate.
    // - Provision-only options (`--base`, `--replace`, `--repl`) are accepted
    //   anywhere and checked after the loop, once the command is known.
    while let Some(arg) = parser.next()? {
        match arg {
            Long("version") => version = true,
            Long("help") | Short('h') => help = true,
            Long("image") => {
                if image_seen {
                    return Err("Duplicate --image".into());
                }
                image_seen = true;
                image = os_to_string(parser.value()?, "--image")?;
                assert_valid_image_name(&image);
            }
            Long("mount-sys-cache") => mount_sys_cache = true,
            Long("no-mount-sys-cache") => mount_sys_cache = false,
            Long("mount-mise") => mount_mise = true,
            Long("no-mount-mise") => mount_mise = false,
            Long(flag @ ("mount-cwd" | "no-mount-cwd")) => {
                mount_cwd = run_only_switch(
                    parsing_provision,
                    &mut run_only,
                    flag,
                    "--mount-cwd",
                    "--no-mount-cwd",
                )?;
            }
            Long(flag @ ("mount-tmux-conf" | "no-mount-tmux-conf")) => {
                mount_tmux_conf = run_only_switch(
                    parsing_provision,
                    &mut run_only,
                    flag,
                    "--mount-tmux-conf",
                    "--no-mount-tmux-conf",
                )?;
            }
            Long(flag @ ("mount-agents-conf" | "no-mount-agents-conf")) => {
                mount_agents_conf = run_only_switch(
                    parsing_provision,
                    &mut run_only,
                    flag,
                    "--mount-agents-conf",
                    "--no-mount-agents-conf",
                )?;
            }
            Long(flag @ ("write-bashrc" | "no-write-bashrc")) => {
                write_bashrc = run_only_switch(
                    parsing_provision,
                    &mut run_only,
                    flag,
                    "--write-bashrc",
                    "--no-write-bashrc",
                )?;
            }
            Long("env") => {
                reject_run_only(parsing_provision, &mut run_only, "--env")?;
                let name = os_to_string(parser.value()?, "--env")?;
                assert!(
                    !env_vars.contains_key(&name),
                    "Duplicate --env value: {name}"
                );
                let value = env::var(&name).unwrap_or_else(|_| {
                    panic!("--env {name} is not set or is not valid UTF-8 in the host environment")
                });
                assert!(
                    !value.is_empty(),
                    "--env {name} is empty in the host environment"
                );
                env_vars.insert(name, value);
            }
            Long("vm-user") => {
                vm_user = VmUser::parse(&os_to_string(parser.value()?, "--vm-user")?)?;
            }
            Long("root-password") => {
                root_password_flag = Some(os_to_string(parser.value()?, "--root-password")?);
            }
            Long("cpus") => cpu_count = parse_cpu_count(&mut parser)?,
            Long("ram") => {
                ram_bytes = parse_ram_size(&mut parser)?;
                ram_explicit = true;
            }
            Long("proxy") => {
                proxy = Some(os_to_string(parser.value()?, "--proxy")?);
            }
            Long("proxy-bypass") => {
                proxy_bypass = Some(os_to_string(parser.value()?, "--proxy-bypass")?);
            }
            Long("proxy-udp") => proxy_udp = Some(true),
            Long("no-proxy-udp") => proxy_udp = Some(false),
            Long("proxy-dns") => proxy_dns = Some(true),
            Long("no-proxy-dns") => proxy_dns = Some(false),
            Long("strict-proxy") => strict_proxy = Some(true),
            Long("no-strict-proxy") => strict_proxy = Some(false),
            Long("no-proxy-vm") => no_proxy_vm = true,
            Long("dns") => {
                dns.push(os_to_string(parser.value()?, "--dns")?);
            }
            Long("ssh-key") => {
                reject_run_only(parsing_provision, &mut run_only, "--ssh-key")?;
                ssh_key = Some(PathBuf::from(parser.value()?));
            }
            Long("git") => {
                reject_run_only(parsing_provision, &mut run_only, "--git")?;
                let value = os_to_string(parser.value()?, "--git")?;
                git_mode = match value.as_str() {
                    "rw" => GitMode::Rw,
                    "ro" => GitMode::Ro,
                    "no" => GitMode::No,
                    _ => return Err("Invalid --git mode".into()),
                };
            }
            Long("publish") | Short('p') => {
                publish.push(os_to_string(parser.value()?, "-p/--publish")?);
            }
            Long("mount") => {
                reject_run_only(parsing_provision, &mut run_only, "--mount")?;
                mounts.push(os_to_string(parser.value()?, "--mount")?);
            }
            Long("network") => {
                let value = os_to_string(parser.value()?, "--network")?;
                network_mode = NetworkMode::parse(&value)?;
            }
            Long("script") => {
                reject_run_only(parsing_provision, &mut run_only, "--script")?;
                let path = os_to_string(parser.value()?, "--script")?;
                let content = fs::read_to_string(&path)
                    .map_err(|err| format!("Failed to read script {path}: {err}"))?;
                login_actions.push(Script {
                    name: path,
                    content,
                });
            }
            Long("send") => {
                reject_run_only(parsing_provision, &mut run_only, "--send")?;
                login_actions.push(Send(os_to_string(parser.value()?, "--send")?));
            }
            Long("expect") => {
                reject_run_only(parsing_provision, &mut run_only, "--expect")?;
                let text = os_to_string(parser.value()?, "--expect")?;
                let timeout = match parser.optional_value() {
                    Some(value) => Duration::from_secs(os_to_string(value, "--expect")?.parse()?),
                    None => DEFAULT_EXPECT_TIMEOUT,
                };
                login_actions.push(Expect { text, timeout });
            }
            Long("base") => {
                if base.is_some() {
                    return Err("Duplicate --base".into());
                }
                base = Some(os_to_string(parser.value()?, "--base")?);
            }
            Long("replace") => replace = true,
            Long("repl") => repl = true,
            Value(value) => {
                let value = os_to_string(
                    value,
                    if parsing_provision {
                        "provisioning script"
                    } else {
                        "argument"
                    },
                )?;
                if parsing_provision {
                    scripts.push(load_provision_script(value)?);
                } else if disk.is_none() && value == "provision" {
                    if let Some(flag) = run_only {
                        return Err(cannot_use_with_provision(flag));
                    }
                    parsing_provision = true;
                } else if disk.is_some() {
                    return Err("Only one disk path may be provided".into());
                } else {
                    disk = Some(PathBuf::from(value));
                }
            }
            _ => return Err(arg.unexpected().into()),
        }
    }

    // Checks that do not depend on the proxy environment variables; the
    // rest happen in `proxy::resolve_vm_proxy`.
    let proxy = proxy::check_cli_proxy_args(
        &proxy::VmProxyArgs {
            proxy: proxy.as_deref(),
            proxy_bypass: proxy_bypass.as_deref(),
            proxy_udp,
            proxy_dns,
            strict_proxy,
            no_proxy_vm,
        },
        &network_mode,
    )?;

    // Apply the provisioning default only when the user did not pass `--ram`.
    if parsing_provision && !ram_explicit {
        ram_bytes = DEFAULT_PROVISION_RAM_BYTES;
    }

    let command = if parsing_provision {
        CliCommand::Provision {
            base,
            image,
            replace,
            repl,
            scripts,
            cpu_count,
            ram_bytes,
        }
    } else if base.is_some() {
        return Err(only_valid_with_provision("--base"));
    } else if replace {
        return Err(only_valid_with_provision("--replace"));
    } else if repl {
        return Err(only_valid_with_provision("--repl"));
    } else {
        CliCommand::Run { disk, image }
    };

    let root_password = if let Some(value) = root_password_flag {
        Some(RootPassword::parse(&value, "--root-password")?)
    } else if let Some(value) = root_password_env {
        Some(RootPassword::parse(value, "VIBE_ROOT_PASSWORD")?)
    } else {
        None
    };

    Ok(CliArgs {
        command,
        version,
        help,
        mount_sys_cache,
        mount_mise,
        mount_cwd,
        mount_tmux_conf,
        mount_agents_conf,
        write_bashrc,
        env: env_vars,
        mounts,
        login_actions,
        network_mode,
        cpu_count,
        ram_bytes,
        proxy,
        proxy_bypass,
        proxy_udp,
        proxy_dns,
        strict_proxy,
        no_proxy_vm,
        dns,
        ssh_key,
        publish,
        git_mode,
        vm_user,
        root_password,
    })
}
