//! `vibe provision`.

use std::path::PathBuf;

use crate::CommandContext;
use crate::cli::{CliArgs, CliCommand};
use crate::images::{ensure_base_image, find_named_image, locate_debian_base, new_image_path};
use crate::provision::provision_image;
use crate::session::{DiskSource, LoginAction, write_bashrc_actions, write_user_bashrc_action};
use crate::session_plan::SessionPlan;
use crate::shares::{create_cache_shares, describe_vm_files, provision_share_notes};
use crate::vm_run::run_vm;
use crate::vm_user::{self, GuestPath, VmUser};

/// Printed when `provision --repl` on an existing image hands over the
/// shell of a non-root vm-user.
pub(crate) const EXISTING_IMAGE_REPL_USER_NOTICE: &str = "Dropping into the vm-user's shell (--repl).\r\n\
Type `exit` to return to the root shell for system-level changes; `exit` there (or `systemctl poweroff`) powers off.";

/// (guest path, source) of the files written into a VM, for the host log.
pub(crate) type WrittenFiles = Vec<(String, String)>;

/// Actions and written files (guest path, source) of `provision --repl` on
/// an existing image. Root gets `/root/.profile` (sourced); a non-root
/// `vm_user` only gets its own `~/.profile`, which its login shell reads.
pub(crate) fn existing_image_repl_actions(
    vm_user: &VmUser,
) -> Result<(Vec<LoginAction>, WrittenFiles), Box<dyn std::error::Error>> {
    let actions = if vm_user.is_root() {
        write_bashrc_actions()?
    } else {
        vec![write_user_bashrc_action(vm_user)?]
    };
    let written_files = vec![
        (
            GuestPath::new(".profile").display(vm_user),
            "built-in".to_string(),
        ),
        ("/root/.bash_logout".to_string(), "built-in".to_string()),
    ];
    Ok((actions, written_files))
}

/// Err when a provision that runs scripts would create a non-root `--vm-user`
/// while root has no password: the user could then become root with `su -`.
pub(crate) fn check_provision_root_password(
    vm_user: &VmUser,
    has_root_password: bool,
) -> Result<(), String> {
    if vm_user.is_root() || has_root_password {
        return Ok(());
    }
    let name = vm_user.as_str();
    Err(format!(
        "--vm-user {name} needs --root-password (or VIBE_ROOT_PASSWORD) when provisioning: without a root password, user '{name}' could become root with 'su -'. Vibe sets the given password for root in the new image."
    ))
}

pub(crate) fn run_provision(
    ctx: CommandContext<'_>,
    args: CliArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let CliCommand::Provision {
        base,
        image,
        replace,
        repl,
        scripts,
        cpu_count,
        ram_bytes,
    } = args.command
    else {
        unreachable!("run_provision needs a provision command");
    };
    let existing_image_raw = find_named_image(ctx.image_dirs, &image);
    let provision_shares = create_cache_shares(ctx.cache_dir, &ctx.cache_specs, &args.vm_user)?;
    let notes = provision_share_notes(args.mount_sys_cache, args.mount_mise);

    // --repl with an existing image and no --replace: boot the image interactively
    // without re-running any provisioning scripts.
    if let Some(existing) = existing_image_raw.as_ref().filter(|_| repl && !replace) {
        println!(
            "Image '{image}' already exists at {}; booting it for interactive use (--repl).",
            existing.display()
        );
        let (login_actions, written_files) = existing_image_repl_actions(&args.vm_user)?;
        let written_files: Vec<(&str, &str)> = written_files
            .iter()
            .map(|(guest, source)| (guest.as_str(), source.as_str()))
            .collect();
        print!(
            "{}",
            describe_vm_files(
                "Provision",
                &provision_shares,
                &written_files,
                &args.vm_user,
                false,
            )
        );
        for note in &notes {
            println!("{note}");
        }
        // No caller actions are user actions; an empty range at the end
        // keeps the `.profile` setup labelled as `[login] setup`.
        let no_user_actions = login_actions.len()..login_actions.len();
        // The account is only checked here; Vibe creates users only
        // while provisioning scripts run.
        let plan = SessionPlan {
            vm_user: args.vm_user.clone(),
            root_prepare: vm_user::check_vm_user_actions(&args.vm_user),
            warn_passwordless_root: !args.vm_user.is_root(),
            switch_user: !args.vm_user.is_root(),
            ..SessionPlan::root(login_actions, no_user_actions)
        };
        return run_vm(
            existing,
            Some(&DiskSource::CachedImage(image.clone())),
            None,
            &plan,
            &provision_shares,
            ctx.prepare_network_backend,
            cpu_count,
            ram_bytes,
            (!args.vm_user.is_root()).then_some(EXISTING_IMAGE_REPL_USER_NOTICE),
            ctx.root_password,
        )
        .map(|_| ());
    }

    check_provision_root_password(&args.vm_user, ctx.root_password.is_some())?;

    // An existing image is replaced in the directory where it was found;
    // a new one is created in the preferred directory.
    let target_image_raw =
        existing_image_raw.unwrap_or_else(|| new_image_path(ctx.image_dirs, &image));

    let base_raw = match base {
        Some(base) if base.contains('/') => PathBuf::from(base),
        Some(base) => {
            let found = find_named_image(ctx.image_dirs, &base).ok_or_else(|| {
                format!(
                    "Base image '{base}' not found in: {}",
                    ctx.image_dirs.searched_list()
                )
            })?;
            println!("Using base image {}", found.display());
            found
        }
        None => {
            let debian = locate_debian_base(ctx.image_dirs);
            ensure_base_image(&debian.raw, &debian.compressed)?;
            println!("Using base image {}", debian.raw.display());
            debian.raw
        }
    };
    if !base_raw.exists() {
        return Err(format!("Base image does not exist: {}", base_raw.display()).into());
    }
    provision_image(
        ctx.image_dirs,
        ctx.cache_dir,
        &base_raw,
        &target_image_raw,
        replace,
        repl,
        &scripts,
        &provision_shares,
        "Provision",
        &notes,
        ctx.prepare_network_backend,
        &ctx.guest_proxy,
        cpu_count,
        ram_bytes,
        ctx.root_password,
        &args.vm_user,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_with_a_non_root_vm_user_needs_a_root_password() {
        let dev = VmUser::parse("dev").unwrap();
        let err = check_provision_root_password(&dev, false).unwrap_err();
        assert!(err.contains("--root-password"), "{err}");
        assert!(err.contains("su -"), "{err}");
        assert!(check_provision_root_password(&dev, true).is_ok());
        assert!(check_provision_root_password(&VmUser::root(), false).is_ok());
    }
}
