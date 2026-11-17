//! Provisioning: creates a named image from a base image by running the
//! provisioning scripts in a VM, then publishes it.
//!
//! The base is copied to a private work file first. `ensure_bootable` runs
//! on that copy, before the file is grown, and never on the run path.
//! Every provision, including ones that were already legacy compatible,
//! clears `/etc/machine-id` before the image is published.

use std::{env, fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc};

use crate::disk_prepare::ensure_bootable;
use crate::guest_template::{replace_region, replace_region_if_present, shell_assign};
use crate::image_dirs::ImageDirs;
use crate::image_probe::{self, MachineIdFact};
use crate::images::{ensure_base_image, locate_debian_base};
use crate::networking::PreparedNetworkBackend;
use crate::proxy::{APT_PROXY_FILE, GuestProxy};
use crate::session::{
    DEFAULT_EXPECT_TIMEOUT,
    LoginAction::{self, *},
    RootPassword, generate_http_proxy_env_export_cmds, set_root_password_actions,
    shell_single_quote, write_bashrc_actions, write_user_bashrc_action,
};
use crate::session_dir::{self, SessionDir};
use crate::session_plan::{SessionPlan, install_bindfs_action, render_install_bindfs};
use crate::shares::{DirectoryShare, cache_share_specs, create_cache_shares, describe_vm_files};
use crate::vm_run::run_vm;
use crate::vm_user::{GuestPath, VmUser, ensure_vm_user_actions};
use crate::{
    BUILTIN_PROVISION_SCRIPTS, BYTES_PER_MB, DEFAULT_CPU_COUNT, DEFAULT_DISK_GB,
    DEFAULT_PROVISION_RAM_BYTES, ProvisionScript,
};

/// Files a provisioning VM that runs scripts gets written into it, as
/// (guest path, source). A non-root `vm_user` also gets its own `.profile`.
pub(crate) fn provision_written_files(vm_user: &VmUser) -> Vec<(String, String)> {
    let mut files = vec![("/root/.profile".to_string(), "built-in".to_string())];
    if !vm_user.is_root() {
        files.push((
            GuestPath::new(".profile").display(vm_user),
            "built-in".to_string(),
        ));
    }
    files.push(("/root/.bash_logout".to_string(), "built-in".to_string()));
    files
}

/// Splits a built-in provisioning script: the lines before it run as root,
/// the lines after it as the `--vm-user`.
pub(crate) const RUN_AS_USER_MARKER: &str = "# vibe:run-as-user";

/// The root part and, when the script has [`RUN_AS_USER_MARKER`], the user
/// part of a built-in provisioning script. Each part carries its own header
/// (`set` options and a source of the provision include): sharing the lines
/// before the marker would also repeat root commands in the user part. The
/// user part only gets the script's `#!` line.
fn split_run_as_user(content: &str) -> (String, Option<String>) {
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        if line.trim_end() == RUN_AS_USER_MARKER {
            let root_part = content[..offset].to_string();
            let rest = &content[offset + line.len()..];
            let user_part = match content.lines().next() {
                Some(shebang) if shebang.starts_with("#!") => format!("{shebang}\n{rest}"),
                _ => rest.to_string(),
            };
            return (root_part, Some(user_part));
        }
        offset += line.len();
    }
    (content.to_string(), None)
}

/// Whether a script part has anything besides comments and blank lines.
fn has_commands(part: &str) -> bool {
    part.lines().any(|line| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with('#')
    })
}

fn is_builtin(script: &ProvisionScript) -> bool {
    BUILTIN_PROVISION_SCRIPTS
        .iter()
        .any(|builtin| builtin.name == script.name && builtin.content == script.content)
}

/// `src/guest/check_vm_user_home.sh`.
const CHECK_VM_USER_HOME_SCRIPT: &str = include_str!("guest/check_vm_user_home.sh");
/// `src/guest/clear_machine_id.sh`.
const CLEAR_MACHINE_ID_SCRIPT: &str = include_str!("guest/clear_machine_id.sh");
/// `src/guest/repl_finalize.sh`.
const REPL_FINALIZE_SCRIPT: &str = include_str!("guest/repl_finalize.sh");
/// `src/guest/provision-env.sh`.
const PROVISION_ENV_SCRIPT: &str = include_str!("guest/provision-env.sh");
/// `src/guest/provision_include.sh`, written as `scripts/include.sh`.
const PROVISION_INCLUDE_SCRIPT: &str = include_str!("guest/provision_include.sh");

fn keep_failed_work() -> bool {
    env::var("VIBE_KEEP_FAILED_WORK").ok().as_deref() == Some("1")
}

/// Error text after a failed provision. With `keep`, it names the work copy
/// that was left behind; otherwise it tells the user how to keep it.
fn failed_work_message(error: &str, work_copy: &Path, keep: bool) -> String {
    if keep {
        format!(
            "{error}\nVIBE_KEEP_FAILED_WORK=1: kept the work copy at {}",
            work_copy.display()
        )
    } else {
        format!(
            "{error}\nThe work copy was removed. Set VIBE_KEEP_FAILED_WORK=1 to keep it for debugging."
        )
    }
}

/// Error for a provision VM that did not finish every action and power off.
/// `pending_action` is the `[action N: ...]` prefix of the action that was
/// running when the VM stopped.
fn incomplete_run_error(
    image_name: &str,
    actions_completed: bool,
    stopped_normally: bool,
    pending_action: Option<&str>,
) -> Option<String> {
    if !actions_completed {
        Some(format!(
            "{} The VM stopped before provisioning of image '{image_name}' completed; the image was not published.",
            pending_action.unwrap_or("[shutdown]")
        ))
    } else if !stopped_normally {
        Some(format!(
            "[shutdown] Provisioning of image '{image_name}' finished, but the VM did not shut down cleanly; the image was not published."
        ))
    } else {
        None
    }
}

fn machine_id_label(fact: &MachineIdFact) -> &'static str {
    match fact {
        MachineIdFact::Missing => "missing",
        MachineIdFact::EmptyFile => "empty",
        MachineIdFact::Valid => "a generated id",
        MachineIdFact::Uninitialized => "uninitialized",
        MachineIdFact::Other => "not empty",
        MachineIdFact::Symlink(_) => "a symbolic link",
    }
}

const PROVISION_SUCCESS_MARKER: &str = "VIBE_PROVISION_SUCCESS";
const DEFAULT_DISK_SIZE: u64 = DEFAULT_DISK_GB * 1024 * BYTES_PER_MB;

/// `default_raw` is the existing default image, or where to create it.
/// The shared cache directories under `cache_dir` are created and mounted only
/// when the image has to be provisioned; the default image is always
/// provisioned with the system caches and mise mounted.
pub(crate) fn ensure_default_image(
    dirs: &ImageDirs,
    default_raw: &Path,
    cache_dir: &Path,
    prepare_network_backend: impl Fn(Option<&Path>) -> PreparedNetworkBackend,
    guest_proxy: &Option<GuestProxy>,
    root_password: Option<Arc<RootPassword>>,
    vm_user: &VmUser,
) -> Result<(), Box<dyn std::error::Error>> {
    // An existing default image is not probed or prepared again.
    if default_raw.exists() {
        println!(
            "The default image {} has been initialized, `provision.sh` will not be run again",
            default_raw.display()
        );
        return Ok(());
    }

    let base = locate_debian_base(dirs);
    ensure_base_image(&base.raw, &base.compressed)?;
    println!("Using base image {}", base.raw.display());

    // Provision with everything, so folks who don't read README have a "it just works" experience.
    let default_provisioning_scripts: Vec<_> = BUILTIN_PROVISION_SCRIPTS
        .iter()
        .filter(|s| s.name != "base")
        .cloned()
        .collect();

    println!("Provisioning default VM with:");
    for s in &default_provisioning_scripts {
        println!("  @{}", s.name);
    }
    println!("If you want a lighter default image, see `vibe provision`");
    if guest_proxy.is_some() {
        println!("{}", GuestProxy::image_note());
    }

    let directory_shares =
        create_cache_shares(cache_dir, &cache_share_specs(true, true, false), vm_user)?;
    provision_image(
        dirs,
        cache_dir,
        &base.raw,
        default_raw,
        false,
        false,
        &default_provisioning_scripts[..],
        &directory_shares,
        "Provision (implicit, for the default image)",
        &[],
        prepare_network_backend,
        guest_proxy,
        DEFAULT_CPU_COUNT,
        DEFAULT_PROVISION_RAM_BYTES,
        root_password,
        vm_user,
    )
}

/// `guest_proxy` is only set in `vznat` mode. It is then saved in the apt
/// configuration of the new image and exported for the provisioning scripts,
/// whether it came from `--proxy` or the environment.
///
/// For root, or for a script that is not built in, the whole script runs as
/// root. Otherwise the built-in script's root part (if it has commands) runs
/// as root and its user part as the `vm_user`.
fn build_provision_script(
    script: &ProvisionScript,
    guest_proxy: &Option<GuestProxy>,
    vm_user: &VmUser,
) -> Vec<LoginAction> {
    let mut content = script.content.to_string();
    if script.name == "base"
        && let Some(guest) = guest_proxy
    {
        let apt_url = shell_single_quote(&guest.apt_url);
        let proxy_code = format!(
            "echo 'Acquire::http::Proxy \"{apt_url}\";' | tee -a {APT_PROXY_FILE};\n\
             echo 'Acquire::https::Proxy \"{apt_url}\";' | tee -a {APT_PROXY_FILE};\n\
             \n\
             {exports}\n",
            exports = generate_http_proxy_env_export_cmds(&guest.url, &guest.no_proxy)
        );

        content = replace_region_if_present(&content, "INJECT_PROXY_CODE", &proxy_code);
    }

    let name = script.name.to_string();
    if vm_user.is_root() || !is_builtin(script) {
        return vec![Script {
            name,
            content,
            file: None,
        }];
    }
    let (root_part, user_part) = split_run_as_user(&content);
    let mut actions = Vec::new();
    if has_commands(&root_part) {
        actions.push(Script {
            name: name.clone(),
            content: root_part,
            file: None,
        });
    }
    if let Some(user_part) = user_part {
        actions.push(UserScript {
            name,
            content: user_part,
            file: None,
        });
    }
    actions
}

/// Renders [`PROVISION_ENV_SCRIPT`]. The root shell sources the file, so the
/// provisioning scripts inherit `DEBIAN_FRONTEND` and the `VIBE_PROVISION_*`
/// variables.
fn render_provision_env(
    image_name: &str,
    base_raw: &str,
    git_sha: &str,
    build_date: &str,
    script_names: &str,
    vm_user: &str,
) -> String {
    let body = format!(
        "export DEBIAN_FRONTEND=noninteractive\n\
         export {}\n\
         export {}\n\
         export {}\n\
         export {}\n\
         export {}\n\
         export {}\n",
        shell_assign("VIBE_PROVISION_IMAGE", image_name),
        shell_assign("VIBE_PROVISION_BASE", base_raw),
        shell_assign("VIBE_GIT_SHA", git_sha),
        shell_assign("VIBE_BUILD_DATE", build_date),
        shell_assign("VIBE_PROVISION_SCRIPTS", script_names),
        shell_assign("VIBE_PROVISION_VM_USER", vm_user),
    );
    replace_region(PROVISION_ENV_SCRIPT, "INJECT_PROVISION_ENV_CONFIG", &body)
        .expect("provision-env.sh has region INJECT_PROVISION_ENV_CONFIG")
}

/// The session of a provision that runs scripts. Before the mounts, root
/// gets `root_password` (applied with `chpasswd`; the password is not
/// embedded in any command) and a non-root `vm_user` is created or checked.
/// After the mounts, `bindfs` is installed, then `.profile` is written and
/// the scripts run (numbered `[action N: ...]`), followed by the finalize
/// and poweroff steps.
///
/// `session` receives `install_bindfs.sh`, `provision-env.sh`, and
/// `scripts/include.sh` (the bytes of `src/guest/provision_include.sh`).
/// The root shell sources `provision-env.sh`, which exports `DEBIAN_FRONTEND`
/// and the `VIBE_PROVISION_*` variables. `install_bindfs.sh` runs in a child
/// process and exports `DEBIAN_FRONTEND` itself. The script wrapper copies
/// `scripts/include.sh` beside each staged script.
#[allow(clippy::too_many_arguments)]
fn build_provision_plan(
    image_name: &str,
    base_raw: &Path,
    scripts: &[&ProvisionScript],
    guest_proxy: &Option<GuestProxy>,
    repl: bool,
    root_password: Option<Arc<RootPassword>>,
    vm_user: &VmUser,
    session: &mut SessionDir,
) -> Result<SessionPlan, Box<dyn std::error::Error>> {
    let mut root_prepare = Vec::new();
    if let Some(password) = root_password {
        root_prepare.extend(set_root_password_actions(password)?);
    }
    root_prepare.extend(ensure_vm_user_actions(vm_user));

    // Every provision installs bindfs, so any image can later run with a
    // non-root `--vm-user`. The install script sets DEBIAN_FRONTEND itself.
    let bindfs_script = render_install_bindfs(guest_proxy);
    let bindfs_file = session
        .write("install_bindfs.sh", &bindfs_script)
        .map_err(|err| {
            format!("[prepare] cannot write install_bindfs.sh into the session directory: {err}")
        })?;
    let mut login_actions = vec![install_bindfs_action(&bindfs_file)];

    // `.profile` sets up PATH and mise activation for the scripts, and is
    // saved with the image. A non-root `vm_user` gets its own copy, read by
    // the login shells of its scripts and sessions.
    login_actions.extend(write_bashrc_actions()?);
    if !vm_user.is_root() {
        login_actions.push(write_user_bashrc_action(vm_user)?);
    }

    let script_names = scripts
        .iter()
        .map(|script| script.name.as_ref())
        .collect::<Vec<_>>()
        .join("\n");
    let env_script = render_provision_env(
        image_name,
        &base_raw.to_string_lossy(),
        env!("GIT_SHA"),
        env!("BUILD_DATE"),
        &script_names,
        vm_user.as_str(),
    );
    let env_file = session
        .write("provision-env.sh", &env_script)
        .map_err(|err| {
            format!("[prepare] cannot write provision-env.sh into the session directory: {err}")
        })?;
    session
        .write("scripts/include.sh", PROVISION_INCLUDE_SCRIPT)
        .map_err(|err| {
            format!("[prepare] cannot write scripts/include.sh into the session directory: {err}")
        })?;
    login_actions.push(Send(format!(" . {}", env_file.as_shell())));

    // `[action N: ...]` numbers the provisioning script actions (`@base`
    // first); a built-in script split for a non-root `vm_user` takes two.
    let first_script = login_actions.len();
    for script in scripts {
        login_actions.extend(build_provision_script(script, guest_proxy, vm_user));
    }
    let user_actions = first_script..login_actions.len();

    if !vm_user.is_root() {
        login_actions.push(Script {
            name: "check-vm-user-home".to_string(),
            content: CHECK_VM_USER_HOME_SCRIPT.to_string(),
            file: None,
        });
    }

    // Clear the template identity after the scripts and before publish.
    // `--repl` defers the clear until poweroff so the interactive session
    // still has the id systemd generated at boot.
    login_actions.push(Script {
        name: "finalize-machine-id".to_string(),
        content: if repl {
            REPL_FINALIZE_SCRIPT.to_string()
        } else {
            CLEAR_MACHINE_ID_SCRIPT.to_string()
        },
        file: None,
    });

    login_actions.push(Send(format!(" echo {PROVISION_SUCCESS_MARKER}")));
    login_actions.push(Expect {
        text: PROVISION_SUCCESS_MARKER.to_string(),
        timeout: DEFAULT_EXPECT_TIMEOUT,
    });

    if !repl {
        login_actions.push(Send(" systemctl poweroff; sleep 100".to_string()));
    }

    // `--repl` ends in the vm-user's session, like a run; `exit` there
    // returns to the root shell.
    Ok(SessionPlan {
        vm_user: vm_user.clone(),
        root_prepare,
        switch_user: repl && !vm_user.is_root(),
        ..SessionPlan::root(login_actions, user_actions)
    })
}

/// Printed when a script-running `--repl` provision hands over the shell.
/// The VM shuts down when the root shell exits or on `systemctl poweroff`.
fn provision_repl_notice(vm_user: &VmUser) -> &'static str {
    if vm_user.is_root() {
        "Provisioning complete. Dropping into interactive shell (--repl).\r\n\
Type `exit` or run `systemctl poweroff` when you are done."
    } else {
        "Provisioning complete. Dropping into the vm-user's shell (--repl).\r\n\
Type `exit` to return to the root shell for system-level changes; `exit` there (or `systemctl poweroff`) powers off and saves the image."
    }
}

/// `cache_dir` holds the session directory for this provision.
/// `stage` names the provision in the host log of shares and written files;
/// `notes` are printed after that log, before the VM starts.
#[allow(clippy::too_many_arguments)]
pub(crate) fn provision_image(
    dirs: &ImageDirs,
    cache_dir: &Path,
    base_raw: &Path,
    image_raw: &Path,
    replace: bool,
    repl: bool,
    extra_scripts: &[ProvisionScript],
    directory_shares: &[DirectoryShare],
    stage: &str,
    notes: &[&str],
    prepare_network_backend: impl Fn(Option<&Path>) -> PreparedNetworkBackend,
    guest_proxy: &Option<GuestProxy>,
    cpu_count: usize,
    ram_bytes: u64,
    root_password: Option<Arc<RootPassword>>,
    vm_user: &VmUser,
) -> Result<(), Box<dyn std::error::Error>> {
    let image_name = image_raw
        .file_stem()
        .expect("image path has a file name")
        .to_string_lossy();
    if image_raw.exists() && !replace {
        return Err(format!(
            "Image '{image_name}' already exists at {}; pass --replace to overwrite it",
            image_raw.display()
        )
        .into());
    }

    let image_dir = image_raw
        .parent()
        .expect("image path has a parent directory");
    fs::create_dir_all(image_dir)?;

    let tmp_raw = image_dir.join(format!("{image_name}.tmp.{}", std::process::id()));
    let _ = fs::remove_file(&tmp_raw);

    let image_path_str = image_raw.to_string_lossy();
    let tmp_file_str = tmp_raw.to_string_lossy();
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        println!("Provisioning image '{image_name}' (path: {image_path_str}; tmp: {tmp_file_str})");

        fs::copy(base_raw, &tmp_raw).map_err(|err| {
            format!(
                "[prepare] cannot copy the base image {} to {tmp_file_str}: {err}",
                base_raw.display()
            )
        })?;
        // fs::copy keeps the base's mode; a read-only base must still give a
        // writable work copy.
        let make_writable = || -> std::io::Result<()> {
            let mut perms = fs::metadata(&tmp_raw)?.permissions();
            perms.set_mode(perms.mode() | 0o200);
            fs::set_permissions(&tmp_raw, perms)
        };
        make_writable()
            .map_err(|err| format!("[prepare] cannot make {tmp_file_str} writable: {err}"))?;
        // Prepare the work copy at its original size. Growing the file first
        // would make the maintenance manifest disagree with the partition table.
        ensure_bootable(&tmp_raw, dirs)?;

        let grow = || -> std::io::Result<()> {
            if fs::metadata(&tmp_raw)?.len() < DEFAULT_DISK_SIZE {
                fs::OpenOptions::new()
                    .write(true)
                    .open(&tmp_raw)?
                    .set_len(DEFAULT_DISK_SIZE)?;
            }
            Ok(())
        };
        grow().map_err(|err| format!("[prepare] cannot grow {tmp_file_str}: {err}"))?;

        let scripts: Vec<&ProvisionScript> = std::iter::once(
            BUILTIN_PROVISION_SCRIPTS
                .iter()
                .find(|script| script.name == "base")
                .expect("base provisioning script is bundled"),
        )
        .chain(extra_scripts.iter())
        .collect();

        let login_password = root_password.clone();
        let mut session = SessionDir::create(cache_dir)
            .map_err(|err| format!("[prepare] cannot create the session directory: {err}"))?;
        let mut plan = match build_provision_plan(
            &image_name,
            base_raw,
            &scripts,
            guest_proxy,
            repl,
            root_password,
            vm_user,
            &mut session,
        ) {
            Ok(plan) => plan,
            Err(err) => {
                session_dir::close_session(session, false)?;
                return Err(err);
            }
        };
        if let Err(err) = session.stage_scripts(&mut plan) {
            session_dir::close_session(session, false)?;
            return Err(err);
        }

        let actions_completed_notice = repl.then(|| provision_repl_notice(vm_user));

        let mut shares = directory_shares.to_vec();
        shares.push(session.share().clone());

        let written_files = provision_written_files(vm_user);
        let written_files: Vec<(&str, &str)> = written_files
            .iter()
            .map(|(path, source)| (path.as_str(), source.as_str()))
            .collect();
        print!(
            "{}",
            describe_vm_files(stage, &shares, &written_files, vm_user, false)
        );
        for note in notes {
            println!("{note}");
        }

        let outcome = run_vm(
            &tmp_raw,
            None,
            None,
            &plan,
            &shares,
            prepare_network_backend,
            cpu_count,
            ram_bytes,
            actions_completed_notice,
            login_password,
        );
        let stopped = outcome.as_ref().is_ok_and(|item| item.stopped_normally);
        if let Err(err) = session_dir::close_session(session, stopped) {
            if let Err(vm_err) = outcome {
                eprintln!("{err}");
                return Err(vm_err);
            }
            return Err(err);
        }
        let outcome = outcome?;
        if let Some(error) = incomplete_run_error(
            &image_name,
            outcome.actions_completed,
            outcome.stopped_normally,
            outcome.pending_action.as_deref(),
        ) {
            return Err(error.into());
        }

        let machine_id = image_probe::probe_disk(&tmp_raw).1.machine_id;
        if machine_id != MachineIdFact::EmptyFile {
            return Err(format!(
                "[verify] provision finished but /etc/machine-id is {}; the image was not published",
                machine_id_label(&machine_id)
            )
            .into());
        }

        fs::rename(&tmp_raw, image_raw).map_err(|err| {
            format!("[verify] cannot publish {tmp_file_str} as {image_path_str}: {err}")
        })?;
        Ok(())
    })();

    result.map_err(|err| {
        let keep = keep_failed_work();
        if !keep {
            let _ = fs::remove_file(&tmp_raw);
        }
        failed_work_message(&err.to_string(), &tmp_raw, keep).into()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        RUN_AS_USER_MARKER, SessionPlan, build_provision_plan, build_provision_script,
        has_commands, incomplete_run_error, render_provision_env, split_run_as_user,
    };
    use crate::proxy::GuestProxy;
    use crate::session::{LoginAction, RootPassword};
    use crate::vm_user::VmUser;
    use crate::{BUILTIN_PROVISION_SCRIPTS, ProvisionScript};
    use std::borrow::Cow;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::Arc;

    fn provision_plan(
        scripts: &[&ProvisionScript],
        repl: bool,
        root_password: Option<Arc<RootPassword>>,
        vm_user: &VmUser,
    ) -> (SessionPlan, String) {
        let cache = std::env::temp_dir().join(format!(
            "vibe-provision-plan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&cache).unwrap();
        let mut session = crate::session_dir::SessionDir::create(&cache).unwrap();
        let plan = build_provision_plan(
            "demo",
            Path::new("/bases/debian.raw"),
            scripts,
            &None,
            repl,
            root_password,
            vm_user,
            &mut session,
        )
        .unwrap();
        let env = std::fs::read_to_string(session.host().join("provision-env.sh")).unwrap();
        let _ = session.finish(true);
        let _ = std::fs::remove_dir_all(&cache);
        (plan, env)
    }

    fn sourced_provision_env(script: &str) -> Vec<String> {
        let command = "\
. /dev/stdin
printf '%s\\0' \"$DEBIAN_FRONTEND\"
printf '%s\\0' \"$VIBE_PROVISION_IMAGE\"
printf '%s\\0' \"$VIBE_PROVISION_BASE\"
printf '%s\\0' \"$VIBE_GIT_SHA\"
printf '%s\\0' \"$VIBE_BUILD_DATE\"
printf '%s\\0' \"$VIBE_PROVISION_SCRIPTS\"
printf '%s\\0' \"$VIBE_PROVISION_VM_USER\"
";
        let mut child = std::process::Command::new("bash")
            .arg("-c")
            .arg(command)
            .env_clear()
            .env("PATH", "/bin:/usr/bin")
            .env("HOME", "/tmp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        let mut parts = text.split('\0');
        (0..7).map(|_| parts.next().unwrap().to_string()).collect()
    }

    fn script_content(name: &'static str, guest_proxy: &Option<GuestProxy>) -> String {
        let script = ProvisionScript {
            name: Cow::Borrowed(name),
            description: Cow::Borrowed(""),
            content: Cow::Borrowed(
                "set -e\n# region INJECT_PROXY_CODE\n# endregion INJECT_PROXY_CODE\napt-get update\n",
            ),
        };
        match build_provision_script(&script, guest_proxy, &VmUser::root()).as_slice() {
            [LoginAction::Script { content, .. }] => content.clone(),
            _ => panic!("expected a script action"),
        }
    }

    #[test]
    fn guest_proxy_is_saved_to_apt_and_exported() {
        let guest = Some(GuestProxy {
            url: "socks5://u:p'w@10.0.0.1:1080".to_string(),
            apt_url: "socks5h://u:p'w@10.0.0.1:1080".to_string(),
            no_proxy: "example.com,10.*".to_string(),
        });
        let content = script_content("base", &guest);
        assert!(content.contains(
            "echo 'Acquire::http::Proxy \"socks5h://u:p'\\''w@10.0.0.1:1080\";' | tee -a /etc/apt/apt.conf.d/99timeout;"
        ));
        assert!(content.contains("Acquire::https::Proxy"));
        assert!(content.contains("export https_proxy='socks5://u:p'\\''w@10.0.0.1:1080';"));
        assert!(
            content.contains(
                "export no_proxy='example.com,10.*'; export NO_PROXY='example.com,10.*';"
            )
        );
        assert!(content.ends_with("# endregion INJECT_PROXY_CODE\napt-get update\n"));
    }

    #[test]
    fn no_guest_proxy_or_other_scripts_write_nothing() {
        let unchanged =
            "set -e\n# region INJECT_PROXY_CODE\n# endregion INJECT_PROXY_CODE\napt-get update\n";
        assert_eq!(script_content("base", &None), unchanged);
        let guest = Some(GuestProxy {
            url: "http://h:1".to_string(),
            apt_url: "http://h:1".to_string(),
            no_proxy: String::new(),
        });
        assert_eq!(script_content("rust", &guest), unchanged);
    }

    #[test]
    fn incomplete_runs_name_the_pending_action_or_shutdown() {
        let stopped =
            incomplete_run_error("demo", false, true, Some("[action 2: script 'base']")).unwrap();
        assert!(
            stopped.starts_with("[action 2: script 'base'] The VM stopped before provisioning"),
            "{stopped}"
        );
        assert!(stopped.contains("not published"));
        let early = incomplete_run_error("demo", false, false, None).unwrap();
        assert!(early.starts_with("[shutdown] "), "{early}");
        let unclean = incomplete_run_error("demo", true, false, None).unwrap();
        assert!(unclean.starts_with("[shutdown] "), "{unclean}");
        assert!(unclean.contains("did not shut down cleanly"));
        assert_eq!(incomplete_run_error("demo", true, true, None), None);
    }

    #[test]
    fn scripted_provision_sets_the_root_password_before_other_scripts() {
        let secret = "s3cret-value";
        let password = Arc::new(RootPassword::parse(secret, "--root-password").unwrap());
        let base = ProvisionScript {
            name: Cow::Borrowed("base"),
            description: Cow::Borrowed(""),
            content: Cow::Borrowed("echo base\n"),
        };
        let extra = ProvisionScript {
            name: Cow::Borrowed("extra"),
            description: Cow::Borrowed(""),
            content: Cow::Borrowed("echo extra\n"),
        };
        let (plan, env) = provision_plan(&[&base, &extra], false, Some(password), &VmUser::root());

        // Root needs no account step; the password is set first.
        assert_eq!(plan.root_prepare.len(), 2);
        assert_eq!(plan.root_prepare[1].label(), "set root password");
        let rendered = format!("{:?}{:?}", plan.root_prepare, plan.root_actions);
        assert!(!rendered.contains(secret), "{rendered}");
        match &plan.root_prepare[0] {
            LoginAction::Send(command) => {
                assert!(command.contains("chpasswd"), "{command}");
                assert!(!command.contains(secret), "{command}");
            }
            other => panic!("expected the chpasswd command, got {other:?}"),
        }
        assert_eq!(plan.root_actions[0].label(), "install bindfs");
        assert!(plan.root_actions.iter().any(|action| matches!(
            action,
            LoginAction::Send(text)
                if text.starts_with(" . ") && text.contains("provision-env.sh")
        )));
        assert!(!plan.root_actions.iter().any(|action| matches!(
            action,
            LoginAction::Send(text) if text.contains("export DEBIAN_FRONTEND")
                || text.contains("export VIBE_PROVISION_")
        )));
        let sourced = sourced_provision_env(&env);
        assert_eq!(sourced[0], "noninteractive");
        assert_eq!(sourced[1], "demo");
        assert_eq!(sourced[2], "/bases/debian.raw");
        assert_eq!(sourced[3], env!("GIT_SHA"));
        assert_eq!(sourced[4], env!("BUILD_DATE"));
        assert_eq!(sourced[5], "base\nextra");
        assert_eq!(sourced[6], "root");
        assert!(plan.user_actions.is_empty());
        assert!(plan.apt_cache_script.is_none() && !plan.warn_passwordless_root);
        assert!(plan.numbered.start > 1);
        match &plan.root_actions[plan.numbered.start] {
            LoginAction::Script { name, .. } => assert_eq!(name, "base"),
            other => panic!("expected the base script, got {other:?}"),
        }
        match &plan.root_actions[plan.numbered.start + 1] {
            LoginAction::Script { name, .. } => assert_eq!(name, "extra"),
            other => panic!("expected the extra script, got {other:?}"),
        }
        assert!(plan.root_actions.iter().any(|action| matches!(
            action,
            LoginAction::Send(text) if text.contains("systemctl poweroff")
        )));
        assert!(env.contains("export VIBE_PROVISION_VM_USER='root'"));

        let (repl, _) = provision_plan(&[&base], true, None, &VmUser::root());
        assert!(repl.root_prepare.is_empty());
        assert!(repl.root_actions.iter().all(|action| match action {
            LoginAction::Send(text) => !text.contains("chpasswd") && !text.contains("VIBE_PW_"),
            _ => action.label() != "set root password",
        }));
        assert!(!repl.root_actions.iter().any(|action| matches!(
            action,
            LoginAction::Send(text) if text.contains("systemctl poweroff")
        )));
    }

    #[test]
    fn scripted_provision_creates_a_non_root_vm_user_after_the_root_password() {
        let password = Arc::new(RootPassword::parse("pw", "--root-password").unwrap());
        let base = ProvisionScript {
            name: Cow::Borrowed("base"),
            description: Cow::Borrowed(""),
            content: Cow::Borrowed("echo base\n"),
        };
        let dev = VmUser::parse("dev").unwrap();
        let (plan, env) = provision_plan(&[&base], false, Some(password), &dev);

        let labels: Vec<String> = plan.root_prepare.iter().map(|a| a.label()).collect();
        assert_eq!(labels[1], "set root password");
        assert_eq!(labels[2], "create vm-user");
        match &plan.root_prepare[2] {
            LoginAction::StatusCommand { vm_user, .. } => assert_eq!(vm_user.as_ref(), Some(&dev)),
            other => panic!("expected the create step, got {other:?}"),
        }
        assert!(plan.root_prepare.iter().any(|action| matches!(
            action,
            LoginAction::Send(text) if text.contains("export VIBE_VM_USER=")
        )));
        assert_eq!(plan.vm_user, dev);
        assert_eq!(plan.root_actions[0].label(), "install bindfs");
        // Scripts still run as root and see every share at its root path.
        assert!(plan.apt_cache_script.is_none());
        assert!(env.contains("export VIBE_PROVISION_VM_USER='dev'"));
        assert_eq!(sourced_provision_env(&env)[6], "dev");
    }

    #[test]
    fn provision_env_source_keeps_quotes_newlines_and_dollars() {
        let image = "o'brien";
        let base = "/bases/o'brien.raw";
        let scripts = "base\nextra's $HOME";
        let script = render_provision_env(image, base, "sha'1", "2026-11-17", scripts, "dev");
        let sourced = sourced_provision_env(&script);
        assert_eq!(
            sourced,
            vec![
                "noninteractive".to_string(),
                image.to_string(),
                base.to_string(),
                "sha'1".to_string(),
                "2026-11-17".to_string(),
                scripts.to_string(),
                "dev".to_string(),
            ]
        );
    }

    #[test]
    fn scripts_split_at_the_run_as_user_marker() {
        let (root, user) = split_run_as_user(
            "#!/bin/bash\nset -e\napt install x\n# vibe:run-as-user\nset -e\nmise install\n",
        );
        assert_eq!(root, "#!/bin/bash\nset -e\napt install x\n");
        assert_eq!(user.as_deref(), Some("#!/bin/bash\nset -e\nmise install\n"));

        let (root, user) =
            split_run_as_user("#!/bin/bash\n# Install X.\n# vibe:run-as-user\nmise use x\n");
        assert!(!has_commands(&root), "{root}");
        assert_eq!(user.as_deref(), Some("#!/bin/bash\nmise use x\n"));

        let whole = "#!/bin/bash\necho '# vibe:run-as-user'\n";
        assert_eq!(split_run_as_user(whole), (whole.to_string(), None));
    }

    fn builtin(name: &str) -> &'static ProvisionScript {
        BUILTIN_PROVISION_SCRIPTS
            .iter()
            .find(|script| script.name == name)
            .unwrap()
    }

    fn kinds(actions: &[LoginAction]) -> Vec<String> {
        actions
            .iter()
            .map(|action| match action {
                LoginAction::Script { .. } => "root".to_string(),
                LoginAction::UserScript { .. } => "user".to_string(),
                other => other.label(),
            })
            .collect()
    }

    #[test]
    fn builtin_scripts_install_user_tools_as_the_vm_user() {
        let dev = VmUser::parse("dev").unwrap();
        for script in BUILTIN_PROVISION_SCRIPTS {
            assert!(
                script
                    .content
                    .contains(&format!("\n{RUN_AS_USER_MARKER}\n")),
                "{} has no run-as-user marker",
                script.name
            );
            // Root runs the whole script.
            match build_provision_script(script, &None, &VmUser::root()).as_slice() {
                [LoginAction::Script { content, .. }] => assert_eq!(content, &script.content),
                other => panic!("{}: {other:?}", script.name),
            }
            let actions = build_provision_script(script, &None, &dev);
            let expected = match script.name.as_ref() {
                "base" | "rust" => vec!["root", "user"],
                _ => vec!["user"],
            };
            assert_eq!(kinds(&actions), expected, "{}", script.name);
            let user_part = match actions.last() {
                Some(LoginAction::UserScript { content, .. }) => content,
                other => panic!("{other:?}"),
            };
            assert!(user_part.starts_with("#!"), "{}", script.name);
            assert!(
                user_part.contains("set -euxo pipefail\n"),
                "{}",
                script.name
            );
            assert!(
                user_part.contains("source \"${VIBE_PROVISION_INCLUDE"),
                "{}",
                script.name
            );
            assert!(!user_part.contains("retryable()"), "{}", script.name);
            assert!(!user_part.contains("apt "), "{}", script.name);
        }

        let guest = Some(GuestProxy {
            url: "http://h:1".to_string(),
            apt_url: "http://h:1".to_string(),
            no_proxy: String::new(),
        });
        match build_provision_script(builtin("base"), &guest, &dev).as_slice() {
            [
                LoginAction::Script { content: root, .. },
                LoginAction::UserScript { content: user, .. },
            ] => {
                assert!(root.contains("export https_proxy='http://h:1'"), "{root}");
                assert!(root.contains("touch /run/vibe/base-first-run"), "{root}");
                assert!(
                    user.contains("trusted_config_paths = [\"${HOME}\"]"),
                    "{user}"
                );
                assert!(user.contains("base-user.done"), "{user}");
            }
            other => panic!("{other:?}"),
        }

        // A script that is not built in always runs as root.
        let custom = ProvisionScript {
            name: Cow::Borrowed("codex"),
            description: Cow::Borrowed(""),
            content: Cow::Owned(format!("{}\necho changed\n", builtin("codex").content)),
        };
        assert_eq!(
            kinds(&build_provision_script(&custom, &None, &dev)),
            ["root"]
        );
    }

    #[test]
    fn non_root_provision_writes_both_profiles_and_checks_home_ownership() {
        let password = Arc::new(RootPassword::parse("pw", "--root-password").unwrap());
        let dev = VmUser::parse("dev").unwrap();
        let scripts = [builtin("base"), builtin("codex")];
        let (plan, _) = provision_plan(&scripts, false, Some(password.clone()), &dev);
        let labels: Vec<String> = plan.root_actions.iter().map(|a| a.label()).collect();
        let profiles: Vec<&String> = plan
            .root_actions
            .iter()
            .filter_map(|action| match action {
                LoginAction::Send(text) if text.contains(".profile") => Some(text),
                _ => None,
            })
            .collect();
        assert!(profiles.iter().any(|text| text.contains("/root/.profile")));
        assert!(
            profiles
                .iter()
                .any(|text| text.contains("runuser -u \"$VIBE_VM_USER\"")
                    && text.contains("\"${VIBE_VM_HOME}\"/.profile"))
        );
        assert_eq!(
            kinds(&plan.root_actions[plan.numbered.clone()]),
            ["root", "user", "user"]
        );
        let check = labels
            .iter()
            .position(|label| label == "script 'check-vm-user-home'")
            .unwrap();
        assert_eq!(check, plan.numbered.end);
        assert_eq!(labels[check + 1], "script 'finalize-machine-id'");
        assert!(!plan.switch_user);

        let (repl, _) = provision_plan(&scripts, true, Some(password), &dev);
        assert!(repl.switch_user);

        let (root, _) = provision_plan(&scripts, true, None, &VmUser::root());
        assert!(!root.switch_user);
        assert_eq!(
            kinds(&root.root_actions[root.numbered.clone()]),
            ["root", "root"]
        );
        assert!(
            !root
                .root_actions
                .iter()
                .any(|a| a.label().contains("check-vm-user-home"))
        );
    }

    struct RetryRun {
        status: i32,
        stdout: String,
        stderr: String,
        sleeps: String,
    }

    /// Runs `body` under `set -euo pipefail` after sourcing the provision
    /// include. A fake `sleep` on `PATH` records its arguments and returns.
    fn run_with_exec_retry(setup: impl Fn(&std::path::Path), body: &str) -> RetryRun {
        let dir = std::env::temp_dir().join(format!(
            "vibe-exec-retry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("include.sh"), super::PROVISION_INCLUDE_SCRIPT).unwrap();
        let sleep_log = dir.join("sleep.log");
        let sleep_bin = dir.join("sleep");
        std::fs::write(
            &sleep_bin,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>\"$VIBE_FAKE_SLEEP_LOG\"\n",
        )
        .unwrap();
        let mut mode = std::fs::metadata(&sleep_bin).unwrap().permissions();
        mode.set_mode(0o755);
        std::fs::set_permissions(&sleep_bin, mode).unwrap();
        setup(&dir);
        let script = format!(
            "set -euo pipefail\nsource '{}'\n{body}\nprintf 'continued\\n'\n",
            dir.join("include.sh").display()
        );
        let path = std::env::var("PATH").unwrap_or_default();
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .current_dir(&dir)
            .env("PATH", format!("{}:{}", dir.display(), path))
            .env("VIBE_FAKE_SLEEP_LOG", &sleep_log)
            .output()
            .unwrap();
        let sleeps = std::fs::read_to_string(&sleep_log).unwrap_or_default();
        let run = RetryRun {
            status: output.status.code().unwrap(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
            sleeps,
        };
        let _ = std::fs::remove_dir_all(&dir);
        run
    }

    #[test]
    fn exec_retry_succeeds_on_the_third_attempt_and_the_script_continues() {
        let run = run_with_exec_retry(
            |dir| {
                let flaky = dir.join("flaky");
                std::fs::write(
                    &flaky,
                    "#!/bin/bash\nn=0\nif [ -f \"$1\" ]; then n=$(cat \"$1\"); fi\nn=$((n + 1))\nprintf '%s\\n' \"$n\" >\"$1\"\n[ \"$n\" -ge 3 ]\n",
                )
                .unwrap();
                let mut mode = std::fs::metadata(&flaky).unwrap().permissions();
                mode.set_mode(0o755);
                std::fs::set_permissions(&flaky, mode).unwrap();
            },
            "exec_retry ./flaky ./count",
        );
        assert_eq!(
            run.status, 0,
            "stdout {}\nstderr {}",
            run.stdout, run.stderr
        );
        assert!(run.stdout.contains("continued\n"), "{}", run.stdout);
        assert!(
            run.stderr
                .contains("Warn:  waiting 10s and try again (error=1)"),
            "{}",
            run.stderr
        );
        assert!(
            run.stderr
                .contains("Warn:  waiting 10s and try again (error=2)"),
            "{}",
            run.stderr
        );
        assert!(!run.stderr.contains("error=3"), "{}", run.stderr);
        assert_eq!(run.sleeps, "10\n10\n");
    }

    #[test]
    fn exec_retry_exits_after_four_failures() {
        let run = run_with_exec_retry(|_dir| {}, "exec_retry false");
        assert_eq!(
            run.status, 1,
            "stdout {}\nstderr {}",
            run.stdout, run.stderr
        );
        assert!(!run.stdout.contains("continued"), "{}", run.stdout);
        assert!(
            run.stderr
                .contains("Warn:  waiting 10s and try again (error=1)"),
            "{}",
            run.stderr
        );
        assert!(
            run.stderr
                .contains("Warn:  waiting 10s and try again (error=2)"),
            "{}",
            run.stderr
        );
        assert!(
            run.stderr
                .contains("Warn:  waiting 10s and try again (error=3)"),
            "{}",
            run.stderr
        );
        assert!(
            run.stderr
                .contains("Error: too many errors and retry attempts, exiting ..."),
            "{}",
            run.stderr
        );
        assert_eq!(run.sleeps, "10\n10\n10\n");
    }
}
