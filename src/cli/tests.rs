use super::*;

fn parse(args: &[&str]) -> Result<CliArgs, String> {
    parse_with_password_env(args, None)
}

fn parse_with_password_env(args: &[&str], env_password: Option<&str>) -> Result<CliArgs, String> {
    parse_cli_from(args.iter().copied(), env_password).map_err(|err| err.to_string())
}

fn parse_err(args: &[&str]) -> String {
    parse_err_with_password_env(args, None)
}

fn parse_err_with_password_env(args: &[&str], env_password: Option<&str>) -> String {
    match parse_cli_from(args.iter().copied(), env_password) {
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
    assert_eq!(args.proxy.as_deref(), Some("http://192.168.1.88:3003"));
    assert_eq!(args.proxy_udp, None);
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
        "--no-proxy-udp",
        "--proxy-bypass",
        "example.com",
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
        "--no-proxy-udp",
        "--proxy-bypass",
        "example.com",
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
        assert_eq!(args.proxy_udp, Some(false));
        assert_eq!(args.proxy_bypass.as_deref(), Some("example.com"));
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

    let version = parse(&["provision", "--version", "--proxy", "http://127.0.0.1:8132"]).unwrap();
    assert!(version.version);
    assert_eq!(version.proxy.as_deref(), Some("http://127.0.0.1:8132"));
}

#[test]
fn run_accepts_its_own_options() {
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
        "--no-mount-cwd",
        "--no-mount-sys-cache",
        "--no-mount-mise",
        "--mount-tmux-conf",
        "--mount-agents-conf",
        "--write-bashrc",
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
    assert!(!args.mount_cwd);
    assert!(!args.mount_sys_cache);
    assert!(!args.mount_mise);
    assert!(args.mount_tmux_conf);
    assert!(args.mount_agents_conf);
    assert!(args.write_bashrc);
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
        (vec!["provision", "--no-mount-cwd"], "--no-mount-cwd"),
        (vec!["--mount-tmux-conf", "provision"], "--mount-tmux-conf"),
        (
            vec!["provision", "--mount-agents-conf"],
            "--mount-agents-conf",
        ),
        (vec!["--no-write-bashrc", "provision"], "--no-write-bashrc"),
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
fn mount_defaults_and_the_last_form_wins() {
    let run = parse(&[]).unwrap();
    assert!(run.mount_sys_cache && run.mount_mise && run.mount_cwd);
    assert!(!run.mount_tmux_conf && !run.mount_agents_conf && !run.write_bashrc);

    let provisioned = parse(&["--no-mount-mise", "provision", "--no-mount-sys-cache"]).unwrap();
    expect_provision(&provisioned);
    assert!(!provisioned.mount_mise);
    assert!(!provisioned.mount_sys_cache);

    let last_wins = parse(&[
        "--mount-agents-conf",
        "--no-mount-agents-conf",
        "--no-mount-mise",
        "--mount-mise",
        "--write-bashrc",
        "--no-write-bashrc",
        "--no-mount-cwd",
        "--mount-cwd",
    ])
    .unwrap();
    assert!(!last_wins.mount_agents_conf);
    assert!(last_wins.mount_mise);
    assert!(!last_wins.write_bashrc);
    assert!(last_wins.mount_cwd);
}

#[test]
fn vm_user_and_root_password_parse_before_or_after_provision() {
    let after = parse(&[
        "provision",
        "--vm-user",
        "dev",
        "--root-password",
        "s3cret-value",
    ])
    .unwrap();
    let before = parse(&[
        "--root-password",
        "s3cret-value",
        "--vm-user",
        "dev",
        "provision",
    ])
    .unwrap();
    for args in [&after, &before] {
        expect_provision(args);
        assert_eq!(args.vm_user, VmUser::parse("dev").unwrap());
        let password = args.root_password.as_ref().unwrap();
        assert_eq!(password.expose(), "s3cret-value");
        assert_eq!(format!("{password:?}"), "***");
        assert!(!format!("{password:?}").contains("s3cret-value"));
    }

    let run = parse(&["--vm-user", "dev", "--root-password", "s3cret-value"]).unwrap();
    assert!(matches!(run.command, CliCommand::Run { .. }));
    assert_eq!(run.vm_user.as_str(), "dev");
    assert_eq!(run.root_password.as_ref().unwrap().expose(), "s3cret-value");
}

#[test]
fn vm_user_root_matches_the_default_and_the_last_value_wins() {
    let defaulted = parse(&[]).unwrap();
    assert_eq!(defaulted.vm_user, VmUser::root());
    assert!(defaulted.root_password.is_none());

    let explicit = parse(&["--vm-user", "root"]).unwrap();
    assert_eq!(explicit.vm_user, defaulted.vm_user);

    let last = parse(&[
        "--vm-user",
        "dev",
        "--vm-user",
        "other",
        "--root-password",
        "first-secret",
        "--root-password",
        "second-secret",
    ])
    .unwrap();
    assert_eq!(last.vm_user.as_str(), "other");
    assert_eq!(last.root_password.unwrap().expose(), "second-secret");
}

#[test]
fn invalid_vm_user_and_root_password_name_the_source() {
    for name in ["", "Dev", "1dev", "dev/user", &"a".repeat(33)] {
        let err = parse_err(&["--vm-user", name]);
        assert!(err.contains("--vm-user"), "{name}: {err}");
    }

    let empty = parse_err(&["--root-password", ""]);
    assert!(empty.contains("--root-password"), "{empty}");
    assert!(!empty.contains("VIBE_ROOT_PASSWORD"), "{empty}");

    let control = parse_err(&["--root-password", "bad\npassword"]);
    assert!(control.contains("--root-password"), "{control}");
    assert!(control.contains("control character"), "{control}");
    assert!(!control.contains("bad\npassword"), "{control}");

    let delete = parse_err(&["--root-password", "bad\u{7f}password"]);
    assert!(delete.contains("--root-password"), "{delete}");

    let from_env = parse_with_password_env(&[], Some("from-env-secret")).unwrap();
    assert_eq!(from_env.root_password.unwrap().expose(), "from-env-secret");
    let utf8 = parse_with_password_env(&["--vm-user", "dev"], Some("pässwörd")).unwrap();
    assert_eq!(utf8.root_password.unwrap().expose(), "pässwörd");

    let flag_wins =
        parse_with_password_env(&["--root-password", "from-flag"], Some("from-env-secret"))
            .unwrap();
    assert_eq!(flag_wins.root_password.unwrap().expose(), "from-flag");

    let bad_env = parse_err_with_password_env(&["provision"], Some(""));
    assert!(bad_env.contains("VIBE_ROOT_PASSWORD"), "{bad_env}");
    assert!(!bad_env.contains("--root-password"), "{bad_env}");

    let bad_env_control = parse_err_with_password_env(&[], Some("line\rbreak"));
    assert!(
        bad_env_control.contains("VIBE_ROOT_PASSWORD"),
        "{bad_env_control}"
    );
    assert!(
        bad_env_control.contains("control character"),
        "{bad_env_control}"
    );

    // An invalid flag is an error even when the environment holds a usable password.
    let invalid_flag = parse_err_with_password_env(&["--root-password", ""], Some("from-env"));
    assert!(invalid_flag.contains("--root-password"), "{invalid_flag}");
    assert!(
        !invalid_flag.contains("VIBE_ROOT_PASSWORD"),
        "{invalid_flag}"
    );
    assert!(!invalid_flag.contains("from-env"), "{invalid_flag}");
}

#[test]
fn proxy_switches_keep_the_last_value() {
    let args = parse(&[
        "--proxy-udp",
        "--no-proxy-udp",
        "--no-proxy-dns",
        "--proxy-dns",
        "--strict-proxy",
        "--no-strict-proxy",
    ])
    .unwrap();
    assert_eq!(args.proxy_udp, Some(false));
    assert_eq!(args.proxy_dns, Some(true));
    assert_eq!(args.strict_proxy, Some(false));
    assert!(!args.no_proxy_vm);

    let args = parse(&["provision", "--no-proxy-vm", "--proxy-bypass", ""]).unwrap();
    assert!(args.no_proxy_vm);
    assert_eq!(args.proxy_bypass.as_deref(), Some(""));
    assert_eq!(args.proxy, None);
}

#[test]
fn proxy_options_are_checked_at_parse_time() {
    // A proxy may come from the environment, so these parse.
    assert!(parse(&["--proxy-udp", "--strict-proxy"]).is_ok());
    assert!(parse(&["--no-proxy-vm", "--no-proxy-udp", "--no-strict-proxy"]).is_ok());

    assert!(
        parse_err(&["--no-proxy-vm", "--strict-proxy"])
            .contains("--strict-proxy cannot be used with --no-proxy-vm")
    );
    assert!(
        parse_err(&["provision", "--network", "vznat", "--proxy-dns"])
            .contains("--proxy-dns cannot be used with --network vznat")
    );
    assert!(parse_err(&["--proxy", "socks4://127.0.0.1:1080"]).contains("socks4"));
    assert!(parse_err(&["--proxy", "ftp://127.0.0.1:21"]).contains("ftp"));
    assert_eq!(
        parse(&["--proxy", "socks5h://127.0.0.1:1080"])
            .unwrap()
            .proxy
            .as_deref(),
        Some("socks5h://127.0.0.1:1080")
    );
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
    assert!(parse_err(&["provision", "--cpus", "0"]).contains("--cpus must be >= 1"));
    assert!(parse_err(&["provision", "--ram", "0"]).contains("--ram must be >= 1"));
    assert!(
        parse_err(&["provision", "--network", "bridge"])
            .contains("Unsupported --network value 'bridge'")
    );
    assert!(parse_err(&["provision", "--base", "a", "--base", "b"]).contains("Duplicate --base"));
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

    assert!(parse_err(&["disk.raw", "provision"]).contains("Only one disk path may be provided"));
}
