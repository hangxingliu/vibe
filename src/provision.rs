//! Provisioning: creates a named image from a base image by running the
//! provisioning scripts in a VM, then publishes it.

use std::{fs, path::Path};

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

const PROVISION_SUCCESS_MARKER: &str = "VIBE_PROVISION_SUCCESS";
const DEFAULT_DISK_SIZE: u64 = DEFAULT_DISK_GB * 1024 * BYTES_PER_MB;

pub(crate) fn ensure_default_image(
    base_raw: &Path,
    base_compressed: &Path,
    default_raw: &Path,
    directory_shares: &[DirectoryShare],
    prepare_network_backend: impl Fn(Option<&Path>) -> PreparedNetworkBackend,
    guest_proxy: &Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
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

        fs::copy(base_raw, &tmp_raw)?;

        let current_size = fs::metadata(&tmp_raw)?.len();
        if current_size < DEFAULT_DISK_SIZE {
            fs::OpenOptions::new()
                .write(true)
                .open(&tmp_raw)?
                .set_len(DEFAULT_DISK_SIZE)?;
        }

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

        for script in scripts {
            login_actions.push(build_provision_script(script, guest_proxy));
        }

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
            directory_shares,
            prepare_network_backend,
            cpu_count,
            ram_bytes,
            actions_completed_notice,
        )?;
        if !outcome.actions_completed || !outcome.stopped_normally {
            return Err(format!(
                "The VM stopped before provisioning of image '{image_name}' completed{}; the image was not published.",
                if outcome.actions_completed {
                    " (it did not shut down cleanly)"
                } else {
                    ""
                }
            )
            .into());
        }

        fs::rename(&tmp_raw, image_raw)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_raw);
    }

    result
}
