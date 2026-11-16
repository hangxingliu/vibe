//! `vibe` (run): the session of a VM run from a project directory.

use std::{collections::HashMap, env, fs, path::Path};

use base64::{Engine, engine::general_purpose};

use crate::cli::{CliArgs, CliCommand};
use crate::images::{ensure_instance_disk, find_named_image, new_image_path};
use crate::provision::ensure_default_image;
use crate::proxy;
use crate::session::{
    DiskSource, LoginAction, LoginAction::*, env_login_actions,
    generate_http_proxy_env_export_cmds, motd_login_action, write_bashrc_actions,
    write_user_bashrc_action, write_user_file_into_vm,
};
use crate::session_plan::SessionPlan;
use crate::shares::{
    DirectoryShare, create_cache_shares, describe_vm_files, project_masks_and_shares,
    run_share_notes, tmux_conf_mounts,
};
use crate::vm_run::run_vm;
use crate::vm_user::{self, GuestPath, VmUser};
use crate::{CommandContext, DEFAULT_IMAGE_NAME, INSTANCE_DIR_NAME, INSTANCE_DISK_IMAGE_NAME};

/// Inputs of [`run_session_plan`] that do not touch the host file system.
pub(crate) struct RunSession<'a> {
    vm_user: &'a VmUser,
    /// The guest path of the shared current directory, if it is shared.
    project_dir: Option<&'a GuestPath>,
    masks: Vec<GuestPath>,
    /// Host `~/.tmux.conf`, written to the target user's `~/.tmux.conf`.
    tmux_conf: Option<String>,
    write_bashrc: bool,
    env: &'a HashMap<String, String>,
    guest_proxy: &'a Option<proxy::GuestProxy>,
    ssh_key: Option<&'a str>,
    shares: &'a [DirectoryShare],
    /// The user's `--send`, `--expect`, and `--script` actions.
    login_actions: Vec<LoginAction>,
}

/// The session of `vibe` (run): root sets up tmux, `.profile`, and SSH for
/// the target user; then the session, in the target user's login shell when
/// it is not root, changes into the project, exports `--env`, shows the share
/// table, and runs the user's login actions.
pub(crate) fn run_session_plan(
    run: RunSession<'_>,
) -> Result<SessionPlan, Box<dyn std::error::Error>> {
    let vm_user = run.vm_user;
    let is_root = vm_user.is_root();
    let mut root_actions = Vec::new();
    if let Some(conf) = run.tmux_conf {
        root_actions.push(Send(write_user_file_into_vm(".tmux.conf", &conf, vm_user)?));
    }
    // Written before the `--env` exports so `.profile` cannot override them.
    // A non-root user's login shell reads it after the switch.
    if run.write_bashrc {
        if is_root {
            root_actions.extend(write_bashrc_actions()?);
        } else {
            root_actions.push(write_user_bashrc_action(vm_user)?);
        }
    }
    // Install SSH public key and start SSH server
    if let Some(key) = run.ssh_key {
        root_actions.push(Send(ssh_key_command(key, vm_user)));
        root_actions.push(Send(" systemctl start ssh".to_string()));
        root_actions.push(Send(" ip -4 -br addr".to_string()));
    }

    let mut user_actions = Vec::new();
    if !is_root {
        // `su --login` starts with a fresh environment.
        user_actions.push(Send(
            " export HISTCONTROL=ignorespace; unset HISTFILE".to_string(),
        ));
    }
    if let Some(project_dir) = run.project_dir {
        user_actions.push(Send(format!(" cd {}", project_dir.to_user_shell(vm_user))));
    }
    user_actions.extend(env_login_actions(run.env));
    // Enable bash history
    user_actions.push(Send(if is_root {
        " export HISTFILE=/root/.bash_history".to_string()
    } else {
        " export HISTFILE=\"$HOME\"/.bash_history".to_string()
    }));
    user_actions.extend(motd_login_action(run.shares, vm_user));
    // Set proxy environment variables for interactive use
    if let Some(guest) = run.guest_proxy {
        user_actions.push(Send(format!(
            " {}",
            generate_http_proxy_env_export_cmds(&guest.url, &guest.no_proxy)
        )));
    }
    // Any user-provided login actions must come after our system ones
    let first = root_actions.len() + user_actions.len();
    let numbered = first..first + run.login_actions.len();
    user_actions.extend(run.login_actions);

    Ok(SessionPlan {
        vm_user: vm_user.clone(),
        root_prepare: vm_user::check_vm_user_actions(vm_user),
        masks: run.masks,
        restrict_apt_cache: !is_root,
        root_actions,
        user_actions,
        numbered,
        warn_passwordless_root: !is_root,
        switch_user: !is_root,
    })
}

/// Root shell command that installs `key` as the target user's only
/// `~/.ssh/authorized_keys`. For a non-root user the directory and file are
/// created by that user (see [`write_user_file_into_vm`]).
pub(crate) fn ssh_key_command(key: &str, vm_user: &VmUser) -> String {
    let b64 = general_purpose::STANDARD.encode(key.trim());
    if vm_user.is_root() {
        return format!(
            " mkdir -p /root/.ssh && echo '{b64}' | base64 --decode > /root/.ssh/authorized_keys && chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys"
        );
    }
    let home = GuestPath::new("").to_shell(vm_user);
    format!(
        " echo '{b64}' | runuser -u \"$VIBE_VM_USER\" -- sh -c 'umask 077 && mkdir -p \"$1/.ssh\" && chmod 700 \"$1/.ssh\" && base64 --decode > \"$1/.ssh/authorized_keys\" && chmod 600 \"$1/.ssh/authorized_keys\"' sh {home}"
    )
}

/// Err when run options cannot be applied to the implicit provision of the
/// default image. The default image is shared by every project, so it is always
/// provisioned with the shared caches mounted; a different layout has to be
/// chosen explicitly with `vibe provision`.
pub(crate) fn check_implicit_provision(
    mount_sys_cache: bool,
    mount_mise: bool,
    vm_user: &VmUser,
    has_root_password: bool,
    default_raw: &Path,
) -> Result<(), String> {
    if !vm_user.is_root() && !has_root_password {
        let name = vm_user.as_str();
        return Err(format!(
            "--vm-user {name} needs a root password while the default image does not exist yet ({}).\n\
Creating the default image creates user '{name}'; without a root password, '{name}' could become root with 'su -'. Either:\n\
  - pass --root-password <password> (or set VIBE_ROOT_PASSWORD), or\n\
  - run `vibe provision --vm-user {name} --root-password <password> [@script ...]` to create the image explicitly.",
            default_raw.display()
        ));
    }
    let given: Vec<&str> = [
        (!mount_sys_cache).then_some("--no-mount-sys-cache"),
        (!mount_mise).then_some("--no-mount-mise"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if given.is_empty() {
        return Ok(());
    }
    let given = given.join(" ");
    Err(format!(
        "{given} cannot be used while the default image does not exist yet ({}).\n\
The default image is created once and shared by all projects. Create it first, either:\n{}\n{}",
        default_raw.display(),
        format_args!("  - run `vibe` once without {given} to create it the default way, or"),
        format_args!(
            "  - run `vibe provision {given} [@script ...]` to create it explicitly (tools are then installed into the image itself)."
        ),
    ))
}

pub(crate) fn run_run(
    ctx: CommandContext<'_>,
    args: CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let CliCommand::Run { disk, image } = args.command else {
        unreachable!("run_run needs a run command");
    };
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
            let default_raw = find_named_image(ctx.image_dirs, &image)
                .unwrap_or_else(|| new_image_path(ctx.image_dirs, &image));
            if !default_raw.exists() {
                check_implicit_provision(
                    args.mount_sys_cache,
                    args.mount_mise,
                    &args.vm_user,
                    ctx.root_password.is_some(),
                    &default_raw,
                )?;
            }
            ensure_default_image(
                ctx.image_dirs,
                &default_raw,
                ctx.cache_dir,
                ctx.prepare_network_backend,
                &ctx.guest_proxy,
                ctx.root_password.clone(),
                &args.vm_user,
            )?;
            default_raw
        } else {
            find_named_image(ctx.image_dirs, &image).ok_or_else(|| {
                format!(
                    "Template image '{image}' not found in: {}",
                    ctx.image_dirs.searched_list()
                )
            })?
        };
        ensure_instance_disk(&instance_raw, &template_raw)?;

        instance_raw
    };

    let vm_user = args.vm_user.clone();
    let mut directory_shares = Vec::new();
    // (guest path, source) of the files written into the VM, for the host log.
    let mut written_files: Vec<(String, String)> =
        vec![("/root/.bash_logout".to_string(), "built-in".to_string())];

    let project_dir = args.mount_cwd.then(|| GuestPath::new(&project_name));
    if args.mount_cwd {
        directory_shares.push(
            DirectoryShare::new(project_root.clone(), &project_name, false, false, true)
                .expect("Project directory must exist"),
        );
    }

    for spec in &args.mounts {
        directory_shares.push(DirectoryShare::from_mount_spec(spec)?);
    }

    let (masks, project_extra_shares) =
        project_masks_and_shares(&directory_shares, args.git_mode, ctx.cache_dir)?;
    directory_shares.extend(project_extra_shares);

    directory_shares.extend(create_cache_shares(
        ctx.cache_dir,
        &ctx.cache_specs,
        &vm_user,
    )?);

    let mut tmux_conf = None;
    if args.mount_tmux_conf {
        let (tmux_share, conf) = tmux_conf_mounts(ctx.home)?;
        directory_shares.extend(tmux_share);
        if let Some((content, host_path)) = conf {
            tmux_conf = Some(content);
            written_files.push((
                GuestPath::new(".tmux.conf").display(&vm_user),
                format!("from {}", host_path.display()),
            ));
        }
    }
    if args.write_bashrc {
        written_files.push((
            GuestPath::new(".profile").display(&vm_user),
            "built-in".to_string(),
        ));
    }

    let ssh_key = args
        .ssh_key
        .as_ref()
        .map(|path| {
            fs::read_to_string(path)
                .map_err(|err| format!("Failed to read SSH key {}: {err}", path.display()))
        })
        .transpose()?;

    let plan = run_session_plan(RunSession {
        vm_user: &vm_user,
        project_dir: project_dir.as_ref(),
        masks,
        tmux_conf,
        write_bashrc: args.write_bashrc,
        env: &args.env,
        guest_proxy: &ctx.guest_proxy,
        ssh_key: ssh_key.as_deref(),
        shares: &directory_shares,
        login_actions: args.login_actions,
    })?;

    let written_files: Vec<(&str, &str)> = written_files
        .iter()
        .map(|(guest, source)| (guest.as_str(), source.as_str()))
        .collect();
    print!(
        "{}",
        describe_vm_files(
            "Run",
            &directory_shares,
            &written_files,
            &vm_user,
            plan.restrict_apt_cache
        )
    );
    for note in run_share_notes(args.mount_sys_cache, args.mount_mise) {
        println!("{note}");
    }

    run_vm(
        &disk_path,
        Some(&disk_source),
        log_to_instance.then_some(instance_dir.as_path()),
        &plan,
        &directory_shares[..],
        ctx.prepare_network_backend,
        args.cpu_count,
        args.ram_bytes,
        None,
        ctx.root_password,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    use crate::session_plan;
    use crate::shares::{GitMode, SYS_CACHE_SHARES, create_cache_shares, project_masks_and_shares};

    fn run_session_test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vibe-run-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let project = dir.join("proj");
        for sub in [".vibe", ".git", "node_modules"] {
            fs::create_dir_all(project.join(sub)).unwrap();
        }
        fs::write(project.join(".env"), "SECRET=1\n").unwrap();
        dir
    }

    /// The whole Run session for `vm_user`, rendered one action per line.
    fn rendered_run_session(dir: &Path, vm_user: &VmUser) -> (Vec<String>, SessionPlan) {
        let project = DirectoryShare::new(dir.join("proj"), "proj", false, false, true).unwrap();
        let mut shares = vec![project];
        let (masks, extra) =
            project_masks_and_shares(&shares, GitMode::No, &dir.join("cache")).unwrap();
        shares.extend(extra);
        shares.extend(
            create_cache_shares(
                &dir.join("cache"),
                &[SYS_CACHE_SHARES[0], SYS_CACHE_SHARES[1]],
                vm_user,
            )
            .unwrap(),
        );
        let env = HashMap::new();
        let project_dir = shares[0].guest.clone();
        let plan = run_session_plan(RunSession {
            vm_user,
            project_dir: Some(&project_dir),
            masks,
            tmux_conf: None,
            write_bashrc: false,
            env: &env,
            guest_proxy: &None,
            ssh_key: None,
            shares: &shares,
            login_actions: vec![Send(" echo hi".into())],
        })
        .unwrap();
        let session = session_plan::expand_session(Vec::new(), &plan, &shares);
        let lines = session
            .actions
            .iter()
            .map(session_plan::tests::render)
            .collect();
        assert_eq!(session.user.len(), 1);
        assert_eq!(
            session_plan::tests::render(&session.actions[session.user.start]),
            " echo hi"
        );
        (lines, plan)
    }

    #[test]
    fn run_session_masks_vibe_and_git_but_not_env() {
        let dir = run_session_test_dir();
        let (lines, plan) = rendered_run_session(&dir, &VmUser::root());
        let text = lines.join("\n");
        assert!(plan.root_prepare.is_empty() && !plan.restrict_apt_cache);
        assert!(
            text.contains("mount -t tmpfs -o mode=0555,uid=0,gid=0 tmpfs /root/proj/.vibe"),
            "{text}"
        );
        assert!(text.contains("tmpfs /root/proj/.git"), "{text}");
        assert!(!text.contains(".env"), "{text}");
        assert!(text.contains("/root/proj/node_modules"), "{text}");
        assert!(text.contains(" cd /root/proj"), "{text}");
        assert!(!text.contains("bindfs"), "{text}");
        // Masks come after every mount, and the project share before its node_modules.
        let last_mount = lines
            .iter()
            .rposition(|l| l.contains("mount --bind"))
            .unwrap();
        let first_mask = lines.iter().position(|l| l.contains("tmpfs")).unwrap();
        assert!(last_mount < first_mask, "{text}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn run_session_for_a_non_root_user_checks_it_and_mounts_below_its_home() {
        let dir = run_session_test_dir();
        let dev = VmUser::parse("dev").unwrap();
        let (lines, plan) = rendered_run_session(&dir, &dev);
        let text = lines.join("\n");
        assert!(plan.warn_passwordless_root && plan.restrict_apt_cache);
        assert!(lines[0].starts_with("[check vm-user]"), "{text}");
        assert!(lines[1].contains("export VIBE_VM_USER="), "{text}");
        assert!(text.contains("\"${VIBE_VM_HOME}\"/proj/.vibe"), "{text}");
        assert!(text.contains("[mount apt cache]"), "{text}");
        assert!(text.contains("\"${VIBE_VM_HOME}\"/.cargo"), "{text}");
        assert!(!text.contains("/root/proj"), "{text}");
        assert!(!text.contains(".env"), "{text}");
        // The user actions run in the user's login shell after the switch,
        // where only $HOME is set.
        let switch = lines
            .iter()
            .position(|l| l == "<switch to vm-user>")
            .unwrap();
        assert_eq!(
            &lines[switch + 1..switch + 4],
            [
                " export HISTCONTROL=ignorespace; unset HISTFILE",
                " cd \"$HOME\"/proj",
                " export HISTFILE=\"$HOME\"/.bash_history",
            ]
        );
        let last_mask = lines.iter().rposition(|l| l.contains("tmpfs")).unwrap();
        assert!(last_mask < switch, "{text}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn run_session_writes_tmux_profile_and_ssh_key_for_the_target_user() {
        let plan_for = |vm_user: &VmUser| {
            run_session_plan(RunSession {
                vm_user,
                project_dir: None,
                masks: Vec::new(),
                tmux_conf: Some("set -g mouse on\n".to_string()),
                write_bashrc: true,
                env: &HashMap::new(),
                guest_proxy: &None,
                ssh_key: Some("ssh-ed25519 AAAA key\n"),
                shares: &[],
                login_actions: Vec::new(),
            })
            .unwrap()
        };
        let render = |plan: &SessionPlan| -> Vec<String> {
            plan.root_actions
                .iter()
                .map(session_plan::tests::render)
                .collect()
        };

        let root = render(&plan_for(&VmUser::root()));
        assert!(root[0].contains("> '/root/.tmux.conf'"), "{root:?}");
        assert!(root[1].contains("> '/root/.profile'"), "{root:?}");
        assert_eq!(root[2], " . /root/.profile");
        assert!(root[3].starts_with(" mkdir -p /root/.ssh && echo '"));
        assert!(!plan_for(&VmUser::root()).switch_user);

        let dev = render(&plan_for(&VmUser::parse("dev").unwrap()));
        assert_eq!(dev.len(), 5, "{dev:?}");
        assert!(
            dev[0].ends_with(" sh \"${VIBE_VM_HOME}\"/.tmux.conf"),
            "{dev:?}"
        );
        assert!(
            dev[1].ends_with(" sh \"${VIBE_VM_HOME}\"/.profile"),
            "{dev:?}"
        );
        assert!(
            dev[2].contains(
                "| runuser -u \"$VIBE_VM_USER\" -- sh -c 'umask 077 && mkdir -p \"$1/.ssh\""
            ),
            "{dev:?}"
        );
        assert!(dev[2].ends_with(" sh \"${VIBE_VM_HOME}\""), "{dev:?}");
        assert_eq!(dev[3], " systemctl start ssh");
        assert!(!dev.iter().any(|l| l.contains("/root/")), "{dev:?}");
    }

    #[test]
    fn implicit_provision_rejects_only_the_given_no_mount_options() {
        let raw = Path::new("/cache/vibe/default.raw");
        let root = VmUser::root();
        assert_eq!(
            check_implicit_provision(true, true, &root, false, raw),
            Ok(())
        );

        let mise = check_implicit_provision(true, false, &root, false, raw).unwrap_err();
        assert!(mise.contains("--no-mount-mise"), "{mise}");
        assert!(!mise.contains("--no-mount-sys-cache"), "{mise}");
        assert!(mise.contains("/cache/vibe/default.raw"), "{mise}");
        assert!(mise.contains("vibe provision --no-mount-mise"), "{mise}");

        let both = check_implicit_provision(false, false, &root, false, raw).unwrap_err();
        assert!(
            both.contains("vibe provision --no-mount-sys-cache --no-mount-mise"),
            "{both}"
        );
    }

    #[test]
    fn implicit_provision_for_a_non_root_vm_user_needs_a_root_password() {
        let raw = Path::new("/cache/vibe/default.raw");
        let dev = VmUser::parse("dev").unwrap();
        let error = check_implicit_provision(true, true, &dev, false, raw).unwrap_err();
        assert!(
            error.starts_with("--vm-user dev needs a root password"),
            "{error}"
        );
        assert!(
            error.contains("vibe provision --vm-user dev --root-password <password>"),
            "{error}"
        );
        assert_eq!(
            check_implicit_provision(true, true, &dev, true, raw),
            Ok(())
        );
    }
}
