use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

mod alpine;
mod cache;
mod cli;
mod control;
mod disk_prepare;
mod ext4ro;
mod gpt;
mod guest_template;
mod help;
mod image_dirs;
mod image_probe;
mod images;
mod maintenance;
mod networking;
mod provision;
mod provision_command;
mod proxy;
mod run_command;
mod seed;
mod session;
mod session_dir;
mod session_plan;
mod shares;
mod signing;
mod vm_checks;
mod vm_config;
mod vm_io;
mod vm_lifecycle;
mod vm_run;
mod vm_user;

use cli::{CliCommand, parse_cli};
use image_dirs::ImageDirs;
use networking::PreparedNetworkBackend;
use session::RootPassword;
use shares::{CacheShare, cache_share_specs, legacy_cache_notice};
use signing::ensure_signed;

const BYTES_PER_MB: u64 = 1024 * 1024;
const DEFAULT_CPU_COUNT: usize = 2;
const DEFAULT_RAM_MB: u64 = 2048;
const DEFAULT_RAM_BYTES: u64 = DEFAULT_RAM_MB * BYTES_PER_MB;
const DEFAULT_DISK_GB: u64 = 12;
const DEFAULT_PROVISION_RAM_BYTES: u64 = 4096 * BYTES_PER_MB;
const DEFAULT_IMAGE_NAME: &str = "default";
const INSTANCE_DIR_NAME: &str = ".vibe";
const INSTANCE_DISK_IMAGE_NAME: &str = "instance.raw";

include!(concat!(env!("OUT_DIR"), "/provisioning.rs"));

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
    let mut args = parse_cli()?;

    let home = env::var("HOME").map(PathBuf::from)?;
    let cache_home = env::var("XDG_CACHE_HOME").map_or_else(|_| home.join(".cache"), PathBuf::from);
    let cache_dir = cache_home.join("vibe");

    if args.version {
        println!("Vibe");
        println!("{}/", help::REPO_URL);
        println!("Git SHA: {}", env!("GIT_SHA"));
        println!("Built: {}", env!("BUILD_DATE"));

        std::process::exit(0);
    }

    let image_dirs = ImageDirs::from_env(cache_dir.clone())?;

    if args.help {
        print!("{}", help::help_text(&cache_dir, &image_dirs));
        std::process::exit(0);
    }

    // Prepare system-wide directories
    fs::create_dir_all(&cache_dir)?;

    ensure_signed();

    println!("{}", image_dirs.describe());

    let usernet_helper_path = cache_dir.join("vibe-usernet");
    let publish = args.publish.clone();

    let host_proxy_env = proxy::HostProxyEnv::from_env();
    proxy::set_download_proxy(proxy::DownloadProxy::resolve(
        args.proxy.as_deref(),
        args.proxy_bypass.as_deref(),
        &host_proxy_env,
    ));
    let proxy_plan = proxy::resolve_vm_proxy(
        &proxy::VmProxyArgs {
            proxy: args.proxy.as_deref(),
            proxy_bypass: args.proxy_bypass.as_deref(),
            proxy_udp: args.proxy_udp,
            proxy_dns: args.proxy_dns,
            strict_proxy: args.strict_proxy,
            no_proxy_vm: args.no_proxy_vm,
        },
        &args.network_mode,
        &host_proxy_env,
    )?;
    println!(
        "{}",
        proxy_plan.describe(matches!(args.command, CliCommand::Provision { .. }))
    );
    let vm_proxy = proxy_plan.vm;
    // In `nat` mode the usernet helper applies the proxy, so nothing is
    // written into the guest. In `vznat` mode the guest gets the proxy
    // variables (and provisioning saves the proxy in the apt configuration).
    let guest_proxy = proxy_plan.guest;
    let root_password = args.root_password.take().map(Arc::new);

    let cache_specs = match &args.command {
        CliCommand::Provision { .. } => {
            cache_share_specs(args.mount_sys_cache, args.mount_mise, false)
        }
        CliCommand::Run { .. } => cache_share_specs(
            args.mount_sys_cache,
            args.mount_mise,
            args.mount_agents_conf,
        ),
    };
    if let Some(notice) = legacy_cache_notice(&cache_dir, &cache_specs, &args.vm_user) {
        println!("{notice}");
    }

    let network_mode = args.network_mode.clone();
    let dns = args.dns.clone();
    let prepare_network_backend = |log_dir: Option<&Path>| {
        network_mode
            .prepare(
                &usernet_helper_path,
                log_dir,
                &publish,
                vm_proxy.as_ref(),
                &dns,
            )
            .unwrap()
    };

    let ctx = CommandContext {
        home: &home,
        cache_dir: &cache_dir,
        image_dirs: &image_dirs,
        cache_specs,
        root_password,
        guest_proxy,
        prepare_network_backend: &prepare_network_backend,
    };
    match args.command {
        CliCommand::Provision { .. } => provision_command::run_provision(ctx, args),
        CliCommand::Run { .. } => run_command::run_run(ctx, args),
    }
}

/// Values shared by the run and provision commands, prepared by [`run_main`].
pub(crate) struct CommandContext<'a> {
    pub(crate) home: &'a Path,
    pub(crate) cache_dir: &'a Path,
    pub(crate) image_dirs: &'a ImageDirs,
    /// The cache directory shares of this command, in mount order.
    pub(crate) cache_specs: Vec<CacheShare>,
    pub(crate) root_password: Option<Arc<RootPassword>>,
    /// Proxy variables written into the guest (`vznat` only).
    pub(crate) guest_proxy: Option<proxy::GuestProxy>,
    /// Starts the network backend; the argument is the log directory.
    pub(crate) prepare_network_backend: &'a dyn Fn(Option<&Path>) -> PreparedNetworkBackend,
}
