//! Runs a VM attached to the host terminal: legacy root login, internal setup,
//! and user login actions.

use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};

use objc2_foundation::*;

use crate::DirectoryShare;
use crate::networking::PreparedNetworkBackend;
use crate::session::{
    LoginAction::{self, *},
    VmOutput, spawn_login_actions_thread, write_file_into_vm,
};
use crate::vm_config::{SHARED_DIRECTORIES_TAG, create_vm_configuration};
use crate::vm_io::{OutputMonitor, create_pipe, spawn_vm_io};
use crate::vm_lifecycle::{request_stop, start_vm};

const LOGIN_EXPECT_TIMEOUT: Duration = Duration::from_secs(120);

// #region scripts for the guest vm
const BASHRC_SCRIPT: &str = include_str!("bashrc.sh");
const BASH_LOGOUT_SCRIPT: &str = include_str!("bash_logout.sh");
// #endregion

pub(crate) fn run_vm(
    disk_path: &Path,
    network_log_dir: Option<&Path>,
    login_actions: &[LoginAction],
    directory_shares: &[DirectoryShare],
    prepare_network_backend: impl Fn(Option<&Path>) -> PreparedNetworkBackend,
    cpu_count: usize,
    ram_bytes: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let disk_path_str = disk_path.to_string_lossy();
    let ram_mib = ram_bytes / 1024 / 1024;
    println!("Run VM (disk: {disk_path_str}; cpu: {cpu_count}; ram: {ram_mib} MiB) ...");

    let (vm_reads_from, we_write_to) = create_pipe();
    let (we_read_from, vm_writes_to) = create_pipe();
    let (resize_reads_from, we_write_resize_to) = create_pipe();
    let mut prepared_network_backend = prepare_network_backend(network_log_dir);
    let config = create_vm_configuration(
        disk_path,
        directory_shares,
        &mut prepared_network_backend,
        vm_reads_from,
        vm_writes_to,
        resize_reads_from,
        cpu_count,
        ram_bytes,
    )?;

    let vm = start_vm(&config)?;

    println!("VM booting...");

    let output_monitor = Arc::new(OutputMonitor::default());
    let io_ctx = spawn_vm_io(
        output_monitor.clone(),
        we_read_from,
        we_write_to,
        we_write_resize_to,
    );

    let mut all_login_actions = vec![
        Expect {
            text: "login: ".to_string(),
            timeout: LOGIN_EXPECT_TIMEOUT,
        },
        Send("root".to_string()),
        Expect {
            text: "Debian GNU/Linux".to_string(),
            timeout: LOGIN_EXPECT_TIMEOUT,
        },
        // Temporarily disable bash history and set commands starting with space to be ignored
        Send(" export HISTCONTROL=ignorespace".to_string()),
        Send(" unset HISTFILE".to_string()),
        // Our terminal is connected via /dev/hvc0 which Debian apparently keeps barebones.
        // We want sane terminal defaults like icrnl (translating carriage returns into newlines)
        Send(" stty -F /dev/hvc0 sane".to_string()),
        // In background, continuously read host terminal resizes sent over hvc1 and update hvc0.
        Send({
            // sorry for this nonsense, the string is so long it angers rustfmt =(
            const S: &str = " sh -c '(while IFS=\" \" read -r rows cols; do stty -F /dev/hvc0 rows \"$rows\" cols \"$cols\"; done) < /dev/hvc1 >/dev/null 2>&1 &'";
            S.to_string()
        }),
        //
        Send(write_file_into_vm("/root/.profile", BASHRC_SCRIPT)?),
        Send(write_file_into_vm(
            "/root/.bash_logout",
            BASH_LOGOUT_SCRIPT,
        )?),
    ];

    if !directory_shares.is_empty() {
        all_login_actions.push(Send(" mkdir -p /mnt/shared".into()));
        all_login_actions.push(Send(format!(
            " mount -t virtiofs {SHARED_DIRECTORIES_TAG} /mnt/shared"
        )));

        for share in directory_shares {
            let staging = format!("/mnt/shared/{}", share.tag());
            let guest = share.guest.to_string_lossy();
            all_login_actions.push(Send(format!(" mkdir -p {guest}")));
            all_login_actions.push(Send(format!(" mount --bind {staging} {guest}")));
        }
    }

    for a in login_actions {
        all_login_actions.push(a.clone());
    }
    all_login_actions.push(
        Send(" [ -x \"${HOME}/.local/bin/mise\" ] && eval \"$(\"${HOME}/.local/bin/mise\" activate bash)\"".to_string()),
    );
    all_login_actions.push(Send(
        " command -v fzf >/dev/null && eval \"$(fzf --bash)\"".to_string(),
    ));

    let (vm_output_tx, vm_output_rx) = mpsc::channel::<VmOutput>();
    let login_actions_thread = spawn_login_actions_thread(
        all_login_actions,
        output_monitor.clone(),
        io_ctx.input_tx.clone(),
        vm_output_tx,
    );

    let mut last_state = None;
    let mut exit_result = Ok(());
    loop {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.2),
            )
        };

        let state = unsafe { vm.state() };
        if last_state != Some(state) {
            //eprintln!("[state] {:?}", state);
            last_state = Some(state);
        }
        match vm_output_rx.try_recv() {
            Ok(VmOutput::LoginActionTimeout { action, timeout }) => {
                exit_result = Err(format!(
                    "Login action ({action}) timed out after {timeout:?}; shutting down."
                ));
                request_stop(&vm);
                break;
            }
            Ok(VmOutput::LoginActionFailed { action, status }) => {
                exit_result = Err(format!(
                    "Login action ({action}) failed with exit code {status}"
                ));
                request_stop(&vm);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        if state != objc2_virtualization::VZVirtualMachineState::Running {
            //eprintln!("VM stopped with state: {:?}", state);
            break;
        }
    }

    let _ = login_actions_thread.join();

    io_ctx.shutdown();

    let output = output_monitor.snapshot();
    exit_result?;
    Ok(output)
}
