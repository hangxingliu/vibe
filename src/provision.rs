//! Provisioning: creates a named image from a base image by running the
//! provisioning scripts in a VM, then publishes it.
//!
//! The base is copied to a private work file first. `ensure_bootable` runs
//! on that copy, before the file is grown, and never on the run path.
//! Every provision, including ones that were already legacy compatible,
//! clears `/etc/machine-id` before the image is published.

use std::{env, fs, os::unix::fs::PermissionsExt, path::Path};

use crate::disk_prepare::ensure_bootable;
use crate::image_probe::{self, MachineIdFact};
use crate::images::ensure_base_image;
use crate::networking::PreparedNetworkBackend;
use crate::session::{
    DEFAULT_EXPECT_TIMEOUT,
    LoginAction::{self, *},
    generate_http_proxy_env_export_cmds, shell_single_quote,
};
use crate::vm_run::run_vm;
use crate::{
    BUILTIN_PROVISION_SCRIPTS, BYTES_PER_MB, DEFAULT_CPU_COUNT, DEFAULT_DISK_GB,
    DEFAULT_PROVISION_RAM_BYTES, DirectoryShare, ProvisionScript,
};

/// Truncate the on-disk machine-id after systemd has committed it.
/// A mount over `/etc/machine-id` means the commit has not happened yet, and
/// truncating it would only clear the overlay.
const CLEAR_MACHINE_ID_SCRIPT: &str = r#"#!/bin/sh
set -eu
overlay=$(findmnt -n /etc/machine-id || true)
if [ -n "$overlay" ]; then
    echo "refusing to publish: /etc/machine-id is still a mount overlay" >&2
    exit 1
fi
if [ -L /etc/machine-id ] || [ ! -f /etc/machine-id ]; then
    echo "refusing to publish: /etc/machine-id is not a regular file" >&2
    exit 1
fi
: > /etc/machine-id
if [ -s /etc/machine-id ]; then
    echo "refusing to publish: /etc/machine-id is not empty" >&2
    exit 1
fi
db=/var/lib/dbus/machine-id
if [ -L "$db" ]; then
    target=$(readlink "$db") || exit 1
    case $target in
        /etc/machine-id|../../../etc/machine-id) ;;
        *)
            echo "refusing to publish: dbus machine-id symlink is not /etc/machine-id" >&2
            exit 1
            ;;
    esac
elif [ -f "$db" ]; then
    : > "$db"
elif [ -e "$db" ]; then
    echo "refusing to publish: dbus machine-id has an unexpected file type" >&2
    exit 1
fi
sync
"#;

/// Install a transient unit that clears machine-id at poweroff. The unit and
/// its script live under `/run` and are not part of the published image.
/// `ExecStop` runs on both `exit` (`.bash_logout` powers off) and
/// `systemctl poweroff`. `/run` is mounted `noexec`, so the script is run
/// through `/bin/sh` instead of being executed directly.
const REPL_FINALIZE_SCRIPT: &str = r#"#!/bin/sh
set -eu
overlay=$(findmnt -n /etc/machine-id || true)
if [ -n "$overlay" ]; then
    echo "refusing to publish: /etc/machine-id is still a mount overlay" >&2
    exit 1
fi
mkdir -p /run/vibe-finalize
cat > /run/vibe-finalize/clear-machine-id.sh << 'EOF'
#!/bin/sh
set -eu
if [ -L /etc/machine-id ] || [ ! -f /etc/machine-id ]; then
    echo "vibe-finalize-template: /etc/machine-id is not a regular file" >&2
    exit 1
fi
: > /etc/machine-id
db=/var/lib/dbus/machine-id
if [ -L "$db" ]; then
    target=$(readlink "$db") || exit 1
    case $target in
        /etc/machine-id|../../../etc/machine-id) ;;
        *)
            echo "vibe-finalize-template: dbus machine-id symlink is not /etc/machine-id" >&2
            exit 1
            ;;
    esac
elif [ -f "$db" ]; then
    : > "$db"
elif [ -e "$db" ]; then
    echo "vibe-finalize-template: dbus machine-id has an unexpected file type" >&2
    exit 1
fi
sync
EOF
systemd-run \
    --unit=vibe-finalize-template \
    -p Type=oneshot \
    -p RemainAfterExit=yes \
    -p 'ExecStop=/bin/sh /run/vibe-finalize/clear-machine-id.sh' \
    /bin/true
systemctl is-active --quiet vibe-finalize-template.service
"#;

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

pub(crate) fn ensure_default_image(
    cache_dir: &Path,
    base_raw: &Path,
    base_compressed: &Path,
    default_raw: &Path,
    directory_shares: &[DirectoryShare],
    prepare_network_backend: impl Fn(Option<&Path>) -> PreparedNetworkBackend,
    guest_proxy: &Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // An existing default image is not probed or prepared again.
    if default_raw.exists() {
        println!(
            "The default image {} has been initialized, `provision.sh` will not be run again",
            default_raw.display()
        );
        return Ok(());
    }

    ensure_base_image(base_raw, base_compressed)?;

    // Provision with everything, so folks who don't read README have a "it just works" experience.
    let default_provisioning_scripts: Vec<_> = BUILTIN_PROVISION_SCRIPTS
        .iter()
        .filter(|s| s.name != "base")
        .cloned()
        .collect();

    println!("Provisioning default VM with:");
    for s in &default_provisioning_scripts {
        println!("  @{}", &s.name);
    }
    println!("If you want a lighter default image, see `vibe provision`");

    provision_image(
        cache_dir,
        base_raw,
        default_raw,
        false,
        false,
        &default_provisioning_scripts[..],
        directory_shares,
        prepare_network_backend,
        guest_proxy,
        DEFAULT_CPU_COUNT,
        DEFAULT_PROVISION_RAM_BYTES,
    )
}

fn build_provision_script(script: &ProvisionScript, guest_proxy: &Option<String>) -> LoginAction {
    let mut content = script.content.to_string();
    if script.name == "base" {
        if let Some(proxy_url) = guest_proxy {
            let proxy_code = format!(
                "echo 'Acquire::http::Proxy \"{proxy_url}\";' | tee -a /etc/apt/apt.conf.d/99timeout;\n\
                 echo 'Acquire::https::Proxy \"{proxy_url}\";' | tee -a /etc/apt/apt.conf.d/99timeout;\n\
                 \n\
                 {exports}\n",
                exports = generate_http_proxy_env_export_cmds(proxy_url)
            );

            let re = regex::Regex::new(
                r"(?m)^#\s+region\s+INJECT_PROXY_CODE\n(?:.*\n)*?#\s+endregion\s+INJECT_PROXY_CODE",
            )
            .expect("valid INJECT_PROXY_CODE region regex");

            content = re
                .replace(
                    &content,
                    format!(
                        "# region INJECT_PROXY_CODE\n{proxy_code}\n# endregion INJECT_PROXY_CODE"
                    ),
                )
                .into_owned();
        }
    }

    Script {
        name: script.name.to_string(),
        content,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn provision_image(
    cache_dir: &Path,
    base_raw: &Path,
    image_raw: &Path,
    replace: bool,
    repl: bool,
    extra_scripts: &[ProvisionScript],
    directory_shares: &[DirectoryShare],
    prepare_network_backend: impl Fn(Option<&Path>) -> PreparedNetworkBackend,
    guest_proxy: &Option<String>,
    cpu_count: usize,
    ram_bytes: u64,
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
        ensure_bootable(&tmp_raw, cache_dir)?;

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

        let mut login_actions = Vec::new();

        let script_names = scripts
            .iter()
            .map(|script| script.name.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        login_actions.push(Send(format!(
            " export VIBE_PROVISION_IMAGE='{}'\n\
export VIBE_PROVISION_BASE='{}'\n\
export VIBE_GIT_SHA='{}'\n\
export VIBE_BUILD_DATE='{}'\n\
export VIBE_PROVISION_SCRIPTS='{}'",
            shell_single_quote(&image_name),
            shell_single_quote(&base_raw.to_string_lossy()),
            env!("GIT_SHA"),
            env!("BUILD_DATE"),
            shell_single_quote(&script_names)
        )));

        // `[action N: ...]` numbers the provisioning scripts (`@base` first).
        let user_actions = login_actions.len()..login_actions.len() + scripts.len();
        for script in scripts {
            login_actions.push(build_provision_script(script, guest_proxy));
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
        });

        login_actions.push(Send(format!(" echo {PROVISION_SUCCESS_MARKER}")));
        login_actions.push(Expect {
            text: PROVISION_SUCCESS_MARKER.to_string(),
            timeout: DEFAULT_EXPECT_TIMEOUT,
        });

        let actions_completed_notice = if repl {
            // Leave the shell open for interactive customisation.
            // The VM will shut down when the user types `exit` or runs a shutdown command.
            Some(
                "Provisioning complete. Dropping into interactive shell (--repl).\r\n\
Type `exit` or run `systemctl poweroff` when you are done.",
            )
        } else {
            login_actions.push(Send(" systemctl poweroff; sleep 100".to_string()));
            None
        };

        let outcome = run_vm(
            &tmp_raw,
            None,
            None,
            &login_actions,
            user_actions,
            directory_shares,
            prepare_network_backend,
            cpu_count,
            ram_bytes,
            actions_completed_notice,
        )?;
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
        CLEAR_MACHINE_ID_SCRIPT, REPL_FINALIZE_SCRIPT, failed_work_message, incomplete_run_error,
        machine_id_label,
    };
    use crate::image_probe::MachineIdFact;
    use std::path::Path;

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
    fn failed_work_message_names_the_kept_copy_only_when_kept() {
        let work = Path::new("/cache/demo.tmp.42");
        let kept = failed_work_message("[verify] bad", work, true);
        assert!(kept.starts_with("[verify] bad\n"));
        assert!(kept.contains("VIBE_KEEP_FAILED_WORK=1: kept the work copy at /cache/demo.tmp.42"));
        let removed = failed_work_message("[verify] bad", work, false);
        assert!(removed.contains("removed"));
        assert!(removed.contains("VIBE_KEEP_FAILED_WORK=1"));
        assert!(!removed.contains("/cache/demo.tmp.42"));
    }

    #[test]
    fn finalize_scripts_clear_machine_id_and_do_not_print_secrets() {
        for script in [CLEAR_MACHINE_ID_SCRIPT, REPL_FINALIZE_SCRIPT] {
            assert!(script.starts_with("#!/bin/sh\n"));
            assert!(script.contains("findmnt -n /etc/machine-id"));
            assert!(script.contains(": > /etc/machine-id"));
            assert!(script.contains("/var/lib/dbus/machine-id"));
            assert!(script.contains("/etc/machine-id|../../../etc/machine-id"));
            assert!(!script.contains("shadow"));
            assert!(!script.contains("unprovisioned"));
            assert!(!script.contains("set -x"));
        }
        assert!(REPL_FINALIZE_SCRIPT.contains("--unit=vibe-finalize-template"));
        assert!(REPL_FINALIZE_SCRIPT.contains("RemainAfterExit=yes"));
        assert!(
            REPL_FINALIZE_SCRIPT
                .contains("'ExecStop=/bin/sh /run/vibe-finalize/clear-machine-id.sh'")
        );
        assert!(REPL_FINALIZE_SCRIPT.contains("/bin/true"));
        assert!(!CLEAR_MACHINE_ID_SCRIPT.contains("systemd-run"));
    }

    #[test]
    fn machine_id_label_does_not_include_file_contents() {
        assert_eq!(machine_id_label(&MachineIdFact::EmptyFile), "empty");
        assert_eq!(
            machine_id_label(&MachineIdFact::Symlink("/tmp/secret".into())),
            "a symbolic link"
        );
        assert!(
            !machine_id_label(&MachineIdFact::Symlink("/tmp/secret".into())).contains("secret")
        );
    }
}
