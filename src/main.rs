use std::{
    borrow::Cow,
    collections::HashMap,
    env,
    ffi::OsString,
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use base64::{Engine, engine::general_purpose};
use lexopt::prelude::*;

mod images;
use images::*;
mod networking;
use networking::*;
mod provision;
use provision::*;
mod session;
use session::*;
mod vm_config;
mod vm_io;
use vm_io::*;
mod vm_lifecycle;
mod vm_run;
use vm_run::*;
mod alpine;
mod cache;
mod control;
mod disk_prepare;
mod ext4ro;
mod gpt;
mod image_dirs;
use image_dirs::ImageDirs;
mod image_probe;
mod maintenance;
mod seed;
mod vm_checks;

const BYTES_PER_MB: u64 = 1024 * 1024;
const DEFAULT_CPU_COUNT: usize = 2;
const DEFAULT_RAM_MB: u64 = 2048;
const DEFAULT_RAM_BYTES: u64 = DEFAULT_RAM_MB * BYTES_PER_MB;
const DEFAULT_DISK_GB: u64 = 12;
const DEFAULT_PROVISION_RAM_BYTES: u64 = 4096 * BYTES_PER_MB;
const DEFAULT_IMAGE_NAME: &str = "default";
const INSTANCE_DIR_NAME: &str = ".vibe";
const INSTANCE_DISK_IMAGE_NAME: &str = "instance.raw";

/// CACHE_NAME:GUEST_VM_PATH
/// these shares will be mounted in provision stage and vm runtime stage
const DEFAULT_CACHE_SHARES: &[&str] = &[
    "mise-cache:/root/.local/share/mise",
    "apt:/var/cache/apt",
    "cargo:/root/.cargo",
    "yarn:/root/.yarn",
    "m2:/root/.m2",
    "go:/root/go",
];

/// Project subfolders masked with tmpfs to prevent host data leaking into the VM.
const MASKED_SUBFOLDERS: &[&str] = &[".vibe"];

/// Project files masked by bind-mounting an empty file over them to prevent host data leaking.
const MASKED_FILES: &[&str] = &[".env"];

include!(concat!(env!("OUT_DIR"), "/provisioning.rs"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitMode {
    Rw,
    Ro,
    No,
}

#[derive(Clone)]
struct DirectoryShare {
    host: PathBuf,
    guest: PathBuf,
    read_only: bool,
    is_project_dir: bool,
}

impl DirectoryShare {
    fn new(
        host: PathBuf,
        mut guest: PathBuf,
        read_only: bool,
        create_host_dir: bool,
        is_project_dir: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !host.exists() {
            if create_host_dir {
                fs::create_dir_all(&host)?;
            } else {
                return Err(format!("Host path does not exist: {}", host.display()).into());
            }
        }
        if !guest.is_absolute() {
            guest = PathBuf::from("/root").join(guest);
        }
        Ok(Self {
            host,
            guest,
            read_only,
            is_project_dir,
        })
    }

    fn from_mount_spec(spec: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 || parts.len() > 3 {
            return Err(format!("Invalid mount spec: {spec}").into());
        }
        let host = PathBuf::from(parts[0]);
        let guest = PathBuf::from(parts[1]);
        let read_only = if parts.len() == 3 {
            match parts[2] {
                "read-only" => true,
                "read-write" => false,
                _ => {
                    return Err(format!(
                        "Invalid mount mode '{}'; expected read-only or read-write",
                        parts[2]
                    )
                    .into());
                }
            }
        } else {
            false
        };
        DirectoryShare::new(host, guest, read_only, false, true)
    }

    fn tag(&self) -> String {
        let path_str = self.host.to_string_lossy();
        let hash = path_str.bytes().fold(5381u64, |h, b| {
            h.wrapping_mul(33).wrapping_add(u64::from(b))
        });
        let base_name = self
            .host
            .file_name()
            .map_or("share".into(), |s| s.to_string_lossy());
        format!("{base_name}_{hash:016x}")
    }
}

fn provisioning_scripts_banner() -> String {
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

fn main() -> std::process::ExitCode {
    // VM checks for the integration tests run here, on the main thread.
    // The normal CLI never sets this.
    if let Ok(which) = env::var("VIBE_VM_CHECK") {
        ensure_signed();
        match vm_checks::run(&which) {
            Ok(()) => return std::process::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("Error: {err}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    // Print errors with Display so multi-line diagnostics (such as the
    // provision hint for boot failures) stay readable and copyable.
    match run_main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run_main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_cli()?;

    let home = env::var("HOME").map(PathBuf::from)?;
    let cache_home = env::var("XDG_CACHE_HOME").map_or_else(|_| home.join(".cache"), PathBuf::from);
    let cache_dir = cache_home.join("vibe");

    if args.version {
        println!("Vibe");
        println!("https://github.com/lynaghk/vibe/");
        println!("Git SHA: {}", env!("GIT_SHA"));
        println!("Built: {}", env!("BUILD_DATE"));

        std::process::exit(0);
    }

    let image_dirs = ImageDirs::from_env(cache_dir.clone())?;

    if args.help {
        println!("Vibe is a quick way to spin up a Linux virtual machine on Mac to sandbox LLM agents.

vibe [OPTIONS] [LOGIN-ACTIONS ...] [path/to/disk.raw]
vibe provision [OPTIONS] [PROVISIONING-OPTIONS] [@built-in | path/to/script.sh ...]

Options may appear before or after `provision`.
`--base`, `--replace`, and `--repl` are only valid with `provision`.
Login actions, `--no-default-mounts`, `--env`, `--mount`, `--git`, and `--ssh-key` are only valid when running a VM.

`path/to/disk.raw`, when given, is booted as-is. Run time does not inspect or modify the disk. An unprepared current Debian image must be copied first with `vibe provision --base`.

Options:

  --help                                                    Print this help message.
  --version                                                 Print the version (commit SHA and build date).
  --image NAME                                              Run: template image (ignored if `{INSTANCE_DIR_NAME}/{INSTANCE_DISK_IMAGE_NAME}` already exists).
                                                            Run time does not inspect or modify the disk. An unprepared current Debian image must be copied first with `vibe provision --base`.
                                                            Provision: name of the image to create (default `default`).
  --cpus COUNT                                              Number of virtual CPUs (default 2).
  --ram MEGABYTES                                           RAM size in megabytes (default 2048 when running, 4096 when provisioning).
  --no-default-mounts                                       Disable all default mounts, including .git and .vibe project subfolder masking.
  --env NAME                                                Export host environment variable NAME inside VM.
                                                            Errors if NAME is unset or empty.
  --mount HOST_PATH:GUEST_PATH[:read-only | :read-write]    Mount HOST_PATH inside VM at GUEST_PATH (default mode `:read-write`)
                                                            Errors if HOST_PATH does not exist.
  --network <nat|vznat>                                     Guest networking mode (default `nat`).
                                                            `nat` uses Vibe's bundled user-mode network stack.
                                                            `vznat` uses Apple's VZNATNetworkDeviceAttachment.
  -p, --publish [udp:][HOST_ADDR:]HOST_PORT:GUEST_PORT      Forward a guest port to the host (repeatable).
                                                            Example: -p 127.0.0.1:8022:22 -p udp:8080:80
  --proxy <URL>                                             Set proxy. Configures apt during provisioning and exports proxy environment variables at login.
                                                            When using the `nat` network mode, all outbound TCP connections from the VM are also
                                                            routed through this proxy (HTTP CONNECT or SOCKS5).
                                                            Loopback, link-local, and private destinations are dialed directly.
  --proxy-udp                                               Also route outbound UDP through the SOCKS5 proxy set via --proxy.
                                                            Requires --proxy to be a socks5:// URL. Has no effect with http:// proxies.
  --dns <ADDR>                                              Custom upstream DNS server for the VM (repeatable; e.g. --dns 8.8.8.8 --dns 1.1.1.1).
                                                            Overrides the system resolver. When --proxy is a socks5:// URL, DNS queries
                                                            to non-local upstreams are tunnelled through the proxy. Local upstreams are
                                                            queried directly. http:// proxies do not carry DNS.
  
  --git <rw | ro | no>                                      How the .git directory is treated (default `ro`).
                                                            rw: share host .git as read-write.
                                                            ro: share host .git as read-only.
                                                            no: mask .git with tmpfs.
							    

Login actions (executed in order after root login, repeatable; not valid with `provision`):

  --script PATH_TO_SCRIPT                                   Run script in VM; stop if it exits non-zero.
  --send SOME_COMMAND                                       Type SOME_COMMAND followed by newline into the VM.
  --expect STRING [timeout-seconds]                         Wait for STRING to appear in console output before executing next login action.
                                                            If STRING does not appear within timeout (default 30 seconds), shutdown VM with error.
  --ssh-key <PUBLIC_KEY_FILE>                               Install SSH public key into VM and start SSH server.

Provisioning creates a new named image by running (built-in) scripts. Options:

  --base NAME_OR_PATH                                       Use this existing image or path/to/image.raw as base for new image (default Debian Stable).
  --image NAME                                              Name for new image (default `default`).
  --replace                                                 Replace existing image with NAME, if one exists.
  --repl                                                    After provisioning (or when image already exists), drop into an interactive shell.
                                                            If the image does not exist or --replace is given: runs all scripts, then leaves
                                                            the shell open so you can customise further before the image is saved.
                                                            If the image already exists (and --replace is not given): boots the existing image
                                                            directly for interactive use without re-running any scripts.
  --cpus COUNT                                              Number of virtual CPUs for the provisioning VM (default 2).
  --ram MEGABYTES                                           RAM size in megabytes for the provisioning VM (default 4096).

A new Debian base is first prepared once in a headless Alpine maintenance VM; older bases and Vibe images are used as-is.
Set the environment variable VIBE_KEEP_FAILED_WORK=1 to keep the work copy and maintenance files of a failed provision for debugging.

Image directories:

  Image names (`--image NAME`, `--base NAME`, the `default` image) and the Debian and Alpine base images are looked up
  in the image directories, in order; the first match is used. Paths given with `/` are used as-is.
  The default image directory is the cache directory. Two optional environment variables add more:

  VIBE_EXTRA_IMAGE_DIR=DIR[:DIR ...]                        Extra image directories (absolute paths, `:` separated), searched after the default one.
                                                            Directories that do not exist are listed as skipped.
  VIBE_PREFERRED_IMAGE_DIR=DIR                              Preferred directory: the default directory or an entry of VIBE_EXTRA_IMAGE_DIR.
                                                            It is searched first and must exist. Downloads and new images are written here;
                                                            an existing image is replaced where it was found.

  Shared caches (apt, cargo, ...), agent configuration, and maintenance logs always stay in the cache directory.
  The lookup order is printed when a VM is run or provisioned.

{}
  
Cache directory:

  {}

{}
",
                 provisioning_scripts_banner(),
                 cache_dir.to_string_lossy().into_owned(),
                 image_dirs.describe(),
        );
        std::process::exit(0);
    }

    // Prepare system-wide directories
    fs::create_dir_all(&cache_dir)?;

    let default_shares = DEFAULT_CACHE_SHARES
        .iter()
        .map(|share| create_cache_share(&cache_dir, share))
        .collect::<Result<Vec<_>, _>>()?;

    ensure_signed();

    println!("{}", image_dirs.describe());

    let usernet_helper_path = cache_dir.join("vibe-usernet");
    let publish = args.publish.clone();
    let proxy = args.proxy.clone();
    let proxy_udp = args.proxy_udp;

    // Do not inject proxy settings into the VM guest if we are using Nat mode,
    // because the usernet helper handles proxying transparently for the VM.
    let guest_proxy = if args.network_mode == NetworkMode::Nat {
        None
    } else {
        args.proxy.clone()
    };

    let prepare_network_backend = |log_dir: Option<&Path>| {
        args.network_mode
            .prepare(
                &usernet_helper_path,
                log_dir,
                &publish,
                proxy.as_deref(),
                proxy_udp,
                &args.dns,
            )
            .unwrap()
    };

    match args.command {
        CliCommand::Provision {
            base,
            image,
            replace,
            repl,
            scripts,
            cpu_count,
            ram_bytes,
        } => {
            let existing_image_raw = find_named_image(&image_dirs, &image);

            // --repl with an existing image and no --replace: boot the image interactively
            // without re-running any provisioning scripts.
            if let Some(existing) = existing_image_raw.as_ref().filter(|_| repl && !replace) {
                println!(
                    "Image '{image}' already exists at {}; booting it for interactive use (--repl).",
                    existing.display()
                );
                return run_vm(
                    existing,
                    Some(&DiskSource::CachedImage(image.clone())),
                    None,
                    &[],
                    0..0,
                    &default_shares,
                    prepare_network_backend,
                    cpu_count,
                    ram_bytes,
                    None,
                )
                .map(|_| ());
            }

            // An existing image is replaced in the directory where it was found;
            // a new one is created in the preferred directory.
            let target_image_raw =
                existing_image_raw.unwrap_or_else(|| new_image_path(&image_dirs, &image));

            let base_raw = match base {
                Some(base) if base.contains('/') => PathBuf::from(base),
                Some(base) => {
                    let found = find_named_image(&image_dirs, &base).ok_or_else(|| {
                        format!(
                            "Base image '{base}' not found in: {}",
                            image_dirs.searched_list()
                        )
                    })?;
                    println!("Using base image {}", found.display());
                    found
                }
                None => {
                    let debian = locate_debian_base(&image_dirs);
                    ensure_base_image(&debian.raw, &debian.compressed)?;
                    println!("Using base image {}", debian.raw.display());
                    debian.raw
                }
            };
            if !base_raw.exists() {
                return Err(format!("Base image does not exist: {}", base_raw.display()).into());
            }
            provision_image(
                &image_dirs,
                &base_raw,
                &target_image_raw,
                replace,
                repl,
                &scripts,
                &default_shares,
                prepare_network_backend,
                &guest_proxy,
                cpu_count,
                ram_bytes,
            )
        }
        CliCommand::Run { disk, image } => {
            let project_root = env::current_dir()?;
            let project_name = project_root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();

            let instance_dir = project_root.join(INSTANCE_DIR_NAME);
            let instance_raw = instance_dir.join(INSTANCE_DISK_IMAGE_NAME);
            // Only persist the networking log for the managed instance disk in `.vibe/`.
            // For external `--disk` there's no natural per-instance directory to write to, so skip logging.
            let log_to_instance = disk.is_none();
            let disk_source = if disk.is_some() {
                DiskSource::External
            } else {
                DiskSource::Instance
            };
            let disk_path = if let Some(path) = disk {
                if !path.exists() {
                    return Err(format!("Disk image does not exist: {}", path.display()).into());
                }
                path.clone()
            } else {
                if image != DEFAULT_IMAGE_NAME && instance_raw.exists() {
                    eprintln!("Ignoring --image {image}, using {}", instance_raw.display());
                }
                let template_raw = if image == DEFAULT_IMAGE_NAME {
                    let default_raw = find_named_image(&image_dirs, &image)
                        .unwrap_or_else(|| new_image_path(&image_dirs, &image));
                    ensure_default_image(
                        &image_dirs,
                        &default_raw,
                        &default_shares,
                        prepare_network_backend,
                        &guest_proxy,
                    )?;
                    default_raw
                } else {
                    find_named_image(&image_dirs, &image).ok_or_else(|| {
                        format!(
                            "Template image '{image}' not found in: {}",
                            image_dirs.searched_list()
                        )
                    })?
                };
                ensure_instance_disk(&instance_raw, &template_raw)?;

                instance_raw
            };

            let mut login_actions = Vec::new();
            let mut directory_shares = Vec::new();

            if !args.no_default_mounts {
                login_actions.push(Send(format!(" cd {project_name}")));

                directory_shares.push(
                    DirectoryShare::new(
                        project_root.clone(),
                        PathBuf::from("/root/").join(&project_name),
                        false,
                        false,
                        true,
                    )
                    .expect("Project directory must exist"),
                );
            }

            for spec in &args.mounts {
                directory_shares.push(DirectoryShare::from_mount_spec(spec)?);
            }

            if !args.no_default_mounts {
                let mut extra_shares = Vec::new();

                // Collect project dirs to mask. We use a separate vector to avoid borrow checker issues
                // and because we want to add .git shares at the end of the list to ensure they are
                // mounted after their parent directories.
                let project_shares: Vec<DirectoryShare> = directory_shares
                    .iter()
                    .filter(|s| s.is_project_dir)
                    .cloned()
                    .collect();

                for share in project_shares {
                    // Discourage read/write of project dir subfolders within the VM.
                    for subfolder in MASKED_SUBFOLDERS {
                        if share.host.join(subfolder).exists() {
                            login_actions.push(Send(format!(
                                " mount -t tmpfs tmpfs {}",
                                share.guest.join(subfolder).display()
                            )));
                        }
                    }

                    if share.host.join("node_modules").is_dir() {
                        let share_tag = share.tag();
                        let node_modules_share = DirectoryShare::new(
                            cache_dir.join(format!("guest_node_modules/{share_tag}")),
                            share.guest.join("node_modules"),
                            false,
                            true,
                            false,
                        );
                        extra_shares.push(node_modules_share?);
                    }

                    // Handle .git directory masking or read-only sharing
                    let git_path = share.host.join(".git");
                    if git_path.exists() {
                        match args.git_mode {
                            GitMode::No => {
                                login_actions.push(Send(format!(
                                    " mount -t tmpfs tmpfs {}",
                                    share.guest.join(".git").display()
                                )));
                            }
                            GitMode::Ro => {
                                // VirtioFS can only share directories; skip if .git is a file
                                // (e.g. git submodules store a gitdir pointer file, not a directory).
                                if git_path.is_dir() {
                                    extra_shares.push(DirectoryShare::new(
                                        git_path,
                                        share.guest.join(".git"),
                                        true,
                                        false,
                                        false,
                                    )?);
                                }
                            }
                            GitMode::Rw => {}
                        }
                    }

                    // Mask individual files by bind-mounting an empty file over them to prevent
                    // host file contents (e.g. secrets) from being readable inside the VM.
                    for filename in MASKED_FILES {
                        if share.host.join(filename).exists() {
                            login_actions.push(Send(format!(
                        r#" [ -n "$__masked_tmp_file" ] || __masked_tmp_file="$(mktemp)"; mount -o bind "$__masked_tmp_file" {}"#,
                        share.guest.join(filename).display()
                    )));
                        }
                    }
                }
                directory_shares.extend(extra_shares);

                directory_shares.extend(default_shares);

                // Add default shares, if they exist
                for share in [
                    create_cache_share(&cache_dir, "codex:/root/.codex"),
                    create_cache_share(&cache_dir, "copilot:/root/.copilot"),
                    create_cache_share(&cache_dir, "claude:/root/.claude"),
                    create_cache_share(&cache_dir, "gemini:/root/.gemini"),
                    create_cache_share(&cache_dir, "pi:/root/.pi"),
                    create_cache_share(&cache_dir, "grok:/root/.grok"),
                    create_cache_share(&cache_dir, "opencode:/root/.config/opencode"),
                    //
                    DirectoryShare::new(
                        home.join(".tmux"),
                        "/root/.tmux".into(),
                        true,
                        false,
                        false,
                    ),
                ]
                .into_iter()
                .flatten()
                {
                    directory_shares.push(share);
                }

                // Provision ~/.tmux.conf
                let tmux_conf = home.join(".tmux.conf");
                if tmux_conf.exists() {
                    login_actions.push(Send(write_file_into_vm(
                        "/root/.tmux.conf",
                        &fs::read_to_string(&tmux_conf)?,
                    )?));
                }
            }

            login_actions.extend(env_login_actions(&args.env));

            // Enable bash history
            login_actions.push(Send(" export HISTFILE=/root/.bash_history".to_string()));

            if let Some(motd_action) = motd_login_action(&directory_shares) {
                login_actions.push(motd_action);
            }

            // Set proxy environment variables for interactive use
            if let Some(url) = &guest_proxy {
                login_actions.push(Send(format!(
                    " {}",
                    generate_http_proxy_env_export_cmds(url)
                )));
            }

            // Install SSH public key and start SSH server
            if let Some(ssh_key_path) = &args.ssh_key {
                let key_content = fs::read_to_string(ssh_key_path).map_err(|err| {
                    format!("Failed to read SSH key {}: {err}", ssh_key_path.display())
                })?;
                let b64 = general_purpose::STANDARD.encode(key_content.trim());
                login_actions.push(Send(format!(
                    " mkdir -p /root/.ssh && echo '{b64}' | base64 --decode > /root/.ssh/authorized_keys && chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys"
                )));
                login_actions.push(Send(" systemctl start ssh".to_string()));
                login_actions.push(Send(" ip -4 -br addr".to_string()));
            }

            // Any user-provided login actions must come after our system ones
            let user_actions = login_actions.len()..login_actions.len() + args.login_actions.len();
            login_actions.extend(args.login_actions);

            run_vm(
                &disk_path,
                Some(&disk_source),
                log_to_instance.then_some(instance_dir.as_path()),
                &login_actions,
                user_actions,
                &directory_shares[..],
                prepare_network_backend,
                args.cpu_count,
                args.ram_bytes,
                None,
            )
            .map(|_| ())
        }
    }
}

struct CliArgs {
    command: CliCommand,
    version: bool,
    help: bool,
    no_default_mounts: bool,
    env: HashMap<String, String>,
    mounts: Vec<String>,
    login_actions: Vec<LoginAction>,
    network_mode: NetworkMode,
    cpu_count: usize,
    ram_bytes: u64,
    proxy: Option<String>,
    proxy_udp: bool,
    dns: Vec<String>,
    ssh_key: Option<PathBuf>,
    publish: Vec<String>,
    git_mode: GitMode,
}

enum CliCommand {
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

fn cannot_use_with_provision(flag: &str) -> Box<dyn std::error::Error> {
    format!("{flag} cannot be used with provision").into()
}

fn only_valid_with_provision(flag: &str) -> Box<dyn std::error::Error> {
    format!("{flag} is only valid with provision").into()
}

fn load_provision_script(name: String) -> Result<ProvisionScript, Box<dyn std::error::Error>> {
    if let Some(builtin) = name.strip_prefix('@') {
        return BUILTIN_PROVISION_SCRIPTS
            .iter()
            .find(|script| script.name == builtin)
            .cloned()
            .ok_or_else(|| -> Box<dyn std::error::Error> {
                format!(
                    "Unknown built-in provisioning script: @{builtin}\n\n{}",
                    provisioning_scripts_banner()
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

fn parse_cli() -> Result<CliArgs, Box<dyn std::error::Error>> {
    parse_cli_from(env::args_os().skip(1))
}

fn parse_cli_from<I>(args: I) -> Result<CliArgs, Box<dyn std::error::Error>>
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
    let mut no_default_mounts = false;
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
    let mut proxy_udp = false;
    let mut dns: Vec<String> = Vec::new();
    let mut ssh_key = None;
    let mut publish = Vec::new();
    let mut git_mode = GitMode::Ro;

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
            Long("no-default-mounts") => {
                reject_run_only(parsing_provision, &mut run_only, "--no-default-mounts")?;
                no_default_mounts = true;
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
            Long("cpus") => cpu_count = parse_cpu_count(&mut parser)?,
            Long("ram") => {
                ram_bytes = parse_ram_size(&mut parser)?;
                ram_explicit = true;
            }
            Long("proxy") => {
                proxy = Some(os_to_string(parser.value()?, "--proxy")?);
            }
            Long("proxy-udp") => {
                proxy_udp = true;
            }
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

    // Validate proxy_udp requires a socks5 proxy
    if proxy_udp {
        match &proxy {
            None => return Err("--proxy-udp requires --proxy to be set".into()),
            Some(url) if !url.to_lowercase().starts_with("socks5://") => {
                return Err(format!(
                    "--proxy-udp is only supported with socks5:// proxies; got {url:?}"
                )
                .into());
            }
            _ => {}
        }
    }

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

    Ok(CliArgs {
        command,
        version,
        help,
        no_default_mounts,
        env: env_vars,
        mounts,
        login_actions,
        network_mode,
        cpu_count,
        ram_bytes,
        proxy,
        proxy_udp,
        dns,
        ssh_key,
        publish,
        git_mode,
    })
}

fn create_cache_share(
    cache_dir: &Path,
    share: &str,
) -> Result<DirectoryShare, Box<dyn std::error::Error>> {
    let (name, guest_path) = share.split_once(':').unwrap();
    DirectoryShare::new(cache_dir.join(name), guest_path.into(), false, true, false)
}

// Ensure the running binary has com.apple.security.virtualization entitlements by checking and, if not, signing and relaunching.
pub fn ensure_signed() {
    let exe = std::env::current_exe().expect("failed to get current exe path");
    let exe_str = exe.to_str().expect("exe path not valid utf-8");

    let has_required_entitlements = {
        let output = Command::new("codesign")
            .args(["-d", "--entitlements", "-", "--xml", exe.to_str().unwrap()])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains("com.apple.security.virtualization")
            }
            _ => false,
        }
    };

    if has_required_entitlements {
        return;
    }

    const ENTITLEMENTS: &str = include_str!("entitlements.plist");
    let entitlements_path = std::env::temp_dir().join("entitlements.plist");
    std::fs::write(&entitlements_path, ENTITLEMENTS).expect("failed to write entitlements");

    let status = Command::new("codesign")
        .args([
            "--sign",
            "-",
            "--force",
            "--entitlements",
            entitlements_path.to_str().unwrap(),
            exe_str,
        ])
        .status();

    let _ = std::fs::remove_file(&entitlements_path);

    match status {
        Ok(s) if s.success() => {
            let err = Command::new(&exe).args(std::env::args_os().skip(1)).exec();
            eprintln!("failed to re-exec after signing: {err}");
            std::process::exit(1);
        }
        Ok(s) => {
            eprintln!("codesign failed with status: {s}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("failed to run codesign: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<CliArgs, String> {
        parse_cli_from(args.iter().copied()).map_err(|err| err.to_string())
    }

    fn parse_err(args: &[&str]) -> String {
        match parse_cli_from(args.iter().copied()) {
            Ok(_) => panic!("expected arguments to be rejected: {args:?}"),
            Err(err) => err.to_string(),
        }
    }

    fn expect_provision(args: &CliArgs) -> (Option<&str>, &str, bool, bool, Vec<&str>, usize, u64) {
        match &args.command {
            CliCommand::Provision {
                base,
                image,
                replace,
                repl,
                scripts,
                cpu_count,
                ram_bytes,
            } => (
                base.as_deref(),
                image,
                *replace,
                *repl,
                scripts.iter().map(|script| script.name.as_ref()).collect(),
                *cpu_count,
                *ram_bytes,
            ),
            CliCommand::Run { .. } => panic!("expected provision command"),
        }
    }

    #[test]
    fn provision_accepts_proxy_after_the_subcommand() {
        let args = parse(&["provision", "--proxy", "192.168.1.88:3003"]).unwrap();
        assert_eq!(args.proxy.as_deref(), Some("192.168.1.88:3003"));
        assert!(!args.proxy_udp);
        assert!(args.dns.is_empty());
        assert_eq!(args.network_mode, NetworkMode::Nat);
        assert_eq!(
            expect_provision(&args),
            (
                None,
                "default",
                false,
                false,
                Vec::<&str>::new(),
                DEFAULT_CPU_COUNT,
                DEFAULT_PROVISION_RAM_BYTES,
            )
        );
    }

    #[test]
    fn shared_options_work_before_or_after_provision() {
        let after = parse(&[
            "provision",
            "--proxy",
            "socks5://127.0.0.1:1080",
            "--proxy-udp",
            "--dns",
            "1.1.1.1",
            "--dns",
            "8.8.8.8",
            "--network",
            "vznat",
            "-p",
            "127.0.0.1:8022:22",
            "--publish",
            "udp:8080:80",
            "--cpus",
            "4",
            "--ram",
            "8192",
            "--image",
            "demo",
            "--base",
            "other",
            "--replace",
            "--repl",
            "@rust",
            "@golang",
        ])
        .unwrap();
        let before = parse(&[
            "--proxy",
            "socks5://127.0.0.1:1080",
            "--proxy-udp",
            "--dns",
            "1.1.1.1",
            "--dns",
            "8.8.8.8",
            "--network",
            "vznat",
            "-p",
            "127.0.0.1:8022:22",
            "--publish",
            "udp:8080:80",
            "--cpus",
            "4",
            "--ram",
            "8192",
            "--image",
            "demo",
            "--base",
            "other",
            "--replace",
            "--repl",
            "provision",
            "@rust",
            "@golang",
        ])
        .unwrap();

        for args in [&after, &before] {
            assert_eq!(args.proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
            assert!(args.proxy_udp);
            assert_eq!(args.dns, ["1.1.1.1", "8.8.8.8"]);
            assert_eq!(args.network_mode, NetworkMode::VzNat);
            assert_eq!(args.publish, ["127.0.0.1:8022:22", "udp:8080:80"]);
            assert_eq!(
                expect_provision(args),
                (
                    Some("other"),
                    "demo",
                    true,
                    true,
                    vec!["rust", "golang"],
                    4,
                    8192 * BYTES_PER_MB,
                )
            );
        }
    }

    #[test]
    fn provision_options_can_follow_scripts_and_later_values_win() {
        let path = std::env::temp_dir().join(format!(
            "vibe-provision-script-{}-{}.sh",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "echo custom\n").unwrap();
        let path_arg = path.to_str().unwrap();

        let args = parse(&[
            "--proxy",
            "http://127.0.0.1:1",
            "provision",
            "@rust",
            path_arg,
            "--proxy",
            "http://127.0.0.1:2",
            "--cpus",
            "3",
        ])
        .unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(args.proxy.as_deref(), Some("http://127.0.0.1:2"));
        let (_, _, _, _, scripts, cpu_count, _) = expect_provision(&args);
        assert_eq!(scripts, ["rust", path_arg]);
        assert_eq!(cpu_count, 3);
    }

    #[test]
    fn omitted_ram_keeps_each_command_default() {
        let run = parse(&[]).unwrap();
        assert!(matches!(run.command, CliCommand::Run { disk: None, .. }));
        assert_eq!(run.cpu_count, DEFAULT_CPU_COUNT);
        assert_eq!(run.ram_bytes, DEFAULT_RAM_BYTES);

        let provisioned = parse(&["provision"]).unwrap();
        let (_, image, _, _, scripts, cpu_count, ram_bytes) = expect_provision(&provisioned);
        assert_eq!(image, DEFAULT_IMAGE_NAME);
        assert!(scripts.is_empty());
        assert_eq!(cpu_count, DEFAULT_CPU_COUNT);
        assert_eq!(ram_bytes, DEFAULT_PROVISION_RAM_BYTES);
        assert_eq!(provisioned.ram_bytes, DEFAULT_PROVISION_RAM_BYTES);

        let overridden = parse(&["--ram", "1024", "--cpus", "8", "provision"]).unwrap();
        let (_, _, _, _, _, cpu_count, ram_bytes) = expect_provision(&overridden);
        assert_eq!(cpu_count, 8);
        assert_eq!(ram_bytes, 1024 * BYTES_PER_MB);
    }

    #[test]
    fn help_and_version_are_accepted_with_provision() {
        let help = parse(&["provision", "-h"]).unwrap();
        assert!(help.help);
        assert!(!help.version);
        expect_provision(&help);

        let version =
            parse(&["provision", "--version", "--proxy", "http://127.0.0.1:8132"]).unwrap();
        assert!(version.version);
        assert_eq!(version.proxy.as_deref(), Some("http://127.0.0.1:8132"));
    }

    #[test]
    fn run_still_accepts_its_own_options() {
        let path = std::env::temp_dir().join(format!(
            "vibe-login-script-{}-{}.sh",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "echo login\n").unwrap();
        let path_arg = path.to_string_lossy().into_owned();

        let args = parse(&[
            "--proxy",
            "http://127.0.0.1:8080",
            "--git",
            "no",
            "--no-default-mounts",
            "--env",
            "PATH",
            "--mount",
            "/tmp:/mnt",
            "--ssh-key",
            "/tmp/id.pub",
            "--script",
            &path_arg,
            "--send",
            "echo hi",
            "--expect",
            "ready",
            "--image",
            "demo",
            "disk.raw",
        ])
        .unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(args.proxy.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(args.git_mode, GitMode::No);
        assert!(args.no_default_mounts);
        assert!(args.env.contains_key("PATH"));
        assert_eq!(args.mounts, ["/tmp:/mnt"]);
        assert_eq!(args.ssh_key, Some(PathBuf::from("/tmp/id.pub")));
        assert!(matches!(
            &args.login_actions[..],
            [
                LoginAction::Script { name, content },
                LoginAction::Send(send),
                LoginAction::Expect { text, timeout },
            ] if name == &path_arg
                && content == "echo login\n"
                && send == "echo hi"
                && text == "ready"
                && *timeout == DEFAULT_EXPECT_TIMEOUT
        ));
        match args.command {
            CliCommand::Run { disk, image } => {
                assert_eq!(disk, Some(PathBuf::from("disk.raw")));
                assert_eq!(image, "demo");
            }
            CliCommand::Provision { .. } => panic!("expected run command"),
        }
    }

    #[test]
    fn provision_rejects_run_only_options_in_either_position() {
        let cases = [
            (vec!["provision", "--mount", "/tmp:/mnt"], "--mount"),
            (vec!["--mount", "/tmp:/mnt", "provision"], "--mount"),
            (
                vec!["provision", "--no-default-mounts"],
                "--no-default-mounts",
            ),
            (
                vec!["--no-default-mounts", "provision"],
                "--no-default-mounts",
            ),
            (vec!["provision", "--env", "PATH"], "--env"),
            (vec!["--env", "PATH", "provision"], "--env"),
            (vec!["provision", "--git", "rw"], "--git"),
            (vec!["--git", "ro", "provision"], "--git"),
            (vec!["provision", "--ssh-key", "id.pub"], "--ssh-key"),
            (vec!["--ssh-key", "id.pub", "provision"], "--ssh-key"),
            (vec!["provision", "--script", "missing.sh"], "--script"),
            (vec!["provision", "--send", "true"], "--send"),
            (vec!["--send", "true", "provision"], "--send"),
            (vec!["provision", "--expect", "ready"], "--expect"),
            (vec!["--expect", "ready", "provision"], "--expect"),
        ];

        for (args, flag) in cases {
            let err = parse_err(&args);
            assert!(
                err.contains(&format!("{flag} cannot be used with provision")),
                "args {args:?} produced {err}"
            );
            assert!(
                !err.contains("invalid option"),
                "args {args:?} produced {err}"
            );
        }
    }

    #[test]
    fn provision_only_options_are_rejected_for_run() {
        assert!(parse_err(&["--repl"]).contains("--repl is only valid with provision"));
        assert!(parse_err(&["--replace"]).contains("--replace is only valid with provision"));
        assert!(parse_err(&["--base", "other"]).contains("--base is only valid with provision"));
    }

    #[test]
    fn provision_reports_invalid_values_and_unknown_options() {
        let missing_proxy = parse_err(&["provision", "--proxy"]);
        assert!(missing_proxy.contains("missing argument"));
        assert!(missing_proxy.contains("--proxy"));

        let bad_udp = parse_err(&[
            "provision",
            "--proxy",
            "http://127.0.0.1:8080",
            "--proxy-udp",
        ]);
        assert!(bad_udp.contains("--proxy-udp is only supported with socks5:// proxies"));

        assert!(
            parse_err(&["provision", "--proxy-udp"])
                .contains("--proxy-udp requires --proxy to be set")
        );
        assert!(parse_err(&["provision", "--cpus", "0"]).contains("--cpus must be >= 1"));
        assert!(parse_err(&["provision", "--ram", "0"]).contains("--ram must be >= 1"));
        assert!(
            parse_err(&["provision", "--network", "bridge"])
                .contains("Unsupported --network value 'bridge'")
        );
        assert!(
            parse_err(&["provision", "--base", "a", "--base", "b"]).contains("Duplicate --base")
        );
        assert!(
            parse_err(&["--image", "demo", "provision", "--image", "other"])
                .contains("Duplicate --image")
        );

        let unknown = parse_err(&["provision", "--not-an-option"]);
        assert!(unknown.contains("invalid option '--not-an-option'"));

        let unknown_script = parse_err(&["provision", "@not-a-script"]);
        assert!(unknown_script.contains("Unknown built-in provisioning script: @not-a-script"));

        let missing_script = parse_err(&["provision", "missing-provision-script.sh"]);
        assert!(missing_script.contains("Failed to read provisioning script"));

        assert!(
            parse_err(&["disk.raw", "provision"]).contains("Only one disk path may be provided")
        );
    }
}
