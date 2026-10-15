//! Login actions typed into the guest console after root login, and the
//! helpers that build the shell commands for them.

use std::{
    collections::HashMap,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use base64::{Engine, engine::general_purpose};

use crate::DirectoryShare;
use crate::vm_io::{OutputMonitor, VmInput};

pub(crate) const DEFAULT_EXPECT_TIMEOUT: Duration = Duration::from_secs(30);
const SCRIPT_ACTION_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const LOGIN_ACTION_INPUT_CHUNK_BYTES: usize = 256;
const LOGIN_ACTION_INPUT_CHUNK_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub(crate) enum LoginAction {
    Expect { text: String, timeout: Duration },
    Send(String),
    Script { name: String, content: String },
}

pub(crate) use LoginAction::*;

pub(crate) fn env_login_actions(env: &HashMap<String, String>) -> Vec<LoginAction> {
    env.iter()
        //leading space to keep env out of bash history; escape single quotes in value
        .map(|(name, value)| Send(format!(" export {name}='{}'", shell_single_quote(value))))
        .collect()
}

pub(crate) fn shell_single_quote(value: &str) -> String {
    value.replace('\'', r"'\''")
}

pub(crate) fn write_file_into_vm(
    vm_path: &str,
    file_content: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let b64 = general_purpose::STANDARD.encode(file_content);
    let quoted_path = shell_single_quote(vm_path);
    let command =
        format!(" echo '{b64}' | base64 --decode > '{quoted_path}'\nchmod +x '{quoted_path}'");
    Ok(command)
}

pub(crate) fn generate_http_proxy_env_export_cmds(proxy_url: &str) -> String {
    let url = shell_single_quote(proxy_url);
    format!(
        "export http_proxy='{url}'; export HTTP_PROXY='{url}'; export https_proxy='{url}'; export HTTPS_PROXY='{url}'; export no_proxy='127.0.0.1,localhost,192.168.*'; export NO_PROXY='127.0.0.1,localhost,192.168.*';"
    )
}

fn script_command_and_status_marker(id: &str, script: &str) -> (String, String) {
    let b64 = general_purpose::STANDARD.encode(script);
    let guest_dir = "/tmp/vibe-scripts";
    let guest_path = format!("{guest_dir}/{id}.sh");

    const STATUS_MARKER_PREFIX: &str = "VIBE_SCRIPT_STATUS_";
    let status_marker = format!("{STATUS_MARKER_PREFIX}{id}");

    // Run the script with stdin from /dev/null so long-running tools spawned by
    // the script cannot consume queued wrapper lines before the parent shell
    // sees them. Print a leading newline before the marker so terminal echo and
    // control sequences cannot merge the echoed printf command with the marker
    // line that wait_for_line_after expects. Reactivate mise after each
    // successful script so later scripts can use newly installed commands.
    let command = format!(
        " free -h; echo 3 > /proc/sys/vm/drop_caches; echo 1 > /proc/sys/vm/compact_memory;\n\
mkdir -p {guest_dir}\n\
echo '{b64}' | base64 --decode >{guest_path}\n\
chmod +x {guest_path}\n\
{guest_path} </dev/null\n\
status=$?\n\
if [ \"$status\" -eq 0 ] && [ -x \"$HOME/.local/bin/mise\" ]; then\n\
eval \"$(\"$HOME/.local/bin/mise\" activate bash)\"\n\
fi\n\
printf '\\n%s%s\\n%s\\n' '{STATUS_MARKER_PREFIX}' '{id}' \"$status\""
    );

    (command, status_marker)
}

pub(crate) fn motd_login_action(directory_shares: &[DirectoryShare]) -> Option<LoginAction> {
    if directory_shares.is_empty() {
        return Some(Send(" clear".into()));
    }

    let host_header = "Host";
    let guest_header = "Guest";
    let mode_header = "Mode";
    let mut host_width = host_header.len();
    let mut guest_width = guest_header.len();
    let mut mode_width = mode_header.len();
    let mut rows = Vec::with_capacity(directory_shares.len());

    for share in directory_shares {
        let host = share.host.to_string_lossy().into_owned();
        let guest = share.guest.to_string_lossy().into_owned();
        let mode = if share.read_only {
            "read-only"
        } else {
            "read-write"
        }
        .to_string();
        host_width = host_width.max(host.len());
        guest_width = guest_width.max(guest.len());
        mode_width = mode_width.max(mode.len());
        rows.push((host, guest, mode));
    }

    let mut output = String::new();
    output.push_str(
        "
░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░ 
░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
 ░▒▓█▓▒▒▓█▓▒░░▒▓█▓▒░▒▓███████▓▒░░▒▓██████▓▒░   
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
  ░▒▓█▓▓█▓▒░ ░▒▓█▓▒░▒▓█▓▒░░▒▓█▓▒░▒▓█▓▒░        
   ░▒▓██▓▒░  ░▒▓█▓▒░▒▓███████▓▒░░▒▓████████▓▒░

",
    );
    output.push_str(&format!(
        "{host_header:<host_width$}  {guest_header:<guest_width$}  {mode_header}\n"
    ));
    output.push_str(&format!(
        "{:-<host_width$}  {:-<guest_width$}  {:-<mode_width$}\n",
        "",
        "",
        "",
        host_width = host_width,
        guest_width = guest_width,
        mode_width = mode_width
    ));

    for (host, guest, mode) in rows {
        output.push_str(&format!(
            "{host:<host_width$}  {guest:<guest_width$}  {mode}\n"
        ));
    }

    let command = format!(" clear && cat <<'VIBE_MOTD'\n{output}\nVIBE_MOTD");
    Some(Send(command))
}

pub(crate) enum VmOutput {
    LoginActionTimeout { action: String, timeout: Duration },
    LoginActionFailed { action: String, status: u8 },
}

pub(crate) fn spawn_login_actions_thread(
    login_actions: Vec<LoginAction>,
    output_monitor: Arc<OutputMonitor>,
    input_tx: mpsc::Sender<VmInput>,
    vm_output_tx: mpsc::Sender<VmOutput>,
) -> thread::JoinHandle<()> {
    fn send_text(input_tx: &mpsc::Sender<VmInput>, mut text: String) {
        // Type the newline so the command is actually submitted.
        text.push('\n');
        // Send text to VM in chunks so that we don't overrun its input buffer.
        for chunk in text.as_bytes().chunks(LOGIN_ACTION_INPUT_CHUNK_BYTES) {
            input_tx
                .send(VmInput::Bytes(chunk.to_vec()))
                .expect("failed to send login action to VM input thread");
            thread::sleep(LOGIN_ACTION_INPUT_CHUNK_DELAY);
        }
    }

    thread::spawn(move || {
        for (index, a) in login_actions.into_iter().enumerate() {
            match a {
                Expect { text, timeout } => {
                    if !output_monitor.wait_for_text(&text, timeout) {
                        let _ = vm_output_tx.send(VmOutput::LoginActionTimeout {
                            action: format!("expect '{text}'"),
                            timeout,
                        });
                        return;
                    }
                }
                Send(text) => {
                    send_text(&input_tx, text);
                }
                Script { name, content } => {
                    let id = format!("script_{index}");
                    let (command, status_marker) = script_command_and_status_marker(&id, &content);
                    send_text(&input_tx, command);
                    match output_monitor.wait_for_line_after(&status_marker, SCRIPT_ACTION_TIMEOUT)
                    {
                        None => {
                            let _ = vm_output_tx.send(VmOutput::LoginActionTimeout {
                                action: format!("script '{name}'"),
                                timeout: SCRIPT_ACTION_TIMEOUT,
                            });
                            return;
                        }
                        Some(status) => {
                            let status = status.trim().parse().unwrap_or(u8::MAX);
                            if status != 0 {
                                let _ = vm_output_tx.send(VmOutput::LoginActionFailed {
                                    action: format!("script '{name}'"),
                                    status,
                                });
                                return;
                            }
                        }
                    }
                }
            }
        }
    })
}
