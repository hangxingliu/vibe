//! Runs a VM attached to the host terminal: legacy root login, internal setup,
//! and user login actions.

use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};

use objc2_foundation::*;
use objc2_virtualization::VZVirtualMachineState;

use crate::DirectoryShare;
use crate::networking::PreparedNetworkBackend;
use crate::session::{
    DiskSource,
    LoginAction::{self, *},
    LoginFailure, VmOutput, legacy_login_prefix, needs_provision_hint, spawn_login_actions_thread,
};
use crate::vm_config::{SHARED_DIRECTORIES_TAG, create_vm_configuration};
use crate::vm_io::{OutputMonitor, create_pipe, spawn_vm_io};
use crate::vm_lifecycle::{request_stop, start_vm};

const LOGIN_EXPECT_TIMEOUT: Duration = Duration::from_secs(120);

/// How a VM run ended when no login action reported an error.
pub(crate) struct RunOutcome {
    /// Every login action, including the caller's, ran to completion.
    pub(crate) actions_completed: bool,
    /// The VM reached the stopped state without an error.
    pub(crate) stopped_normally: bool,
    /// Console output that was not consumed by login actions.
    #[allow(dead_code)]
    pub(crate) output: String,
}

/// Boots `disk_path` attached to the host terminal, logs in as root, runs the
/// internal setup and then `login_actions`.
///
/// Returns an error when a login action fails or times out, and when the VM
/// stops before the root login completed. `disk_source` selects the
/// `vibe provision` suggestion attached to boot and login errors; `None`
/// omits it. `actions_completed_notice` is printed once all actions finished.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_vm(
    disk_path: &Path,
    disk_source: Option<&DiskSource>,
    network_log_dir: Option<&Path>,
    login_actions: &[LoginAction],
    directory_shares: &[DirectoryShare],
    prepare_network_backend: impl Fn(Option<&Path>) -> PreparedNetworkBackend,
    cpu_count: usize,
    ram_bytes: u64,
    actions_completed_notice: Option<&str>,
) -> Result<RunOutcome, Box<dyn std::error::Error>> {
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

    let mut all_login_actions = legacy_login_prefix(LOGIN_EXPECT_TIMEOUT)?;

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

    let login_error = |failure: LoginFailure| -> String {
        let mut message = failure.describe(disk_path);
        if let Some(source) = disk_source.filter(|_| failure.suggests_provision()) {
            message.push('\n');
            message.push_str(&needs_provision_hint(disk_path, source));
        }
        message
    };

    let mut login_ready = false;
    let mut actions_completed = false;
    // Handles one message from the actions thread; returns the error that
    // ends the run, if any.
    let mut handle_output = |output: VmOutput| -> Option<String> {
        let error = match output {
            VmOutput::LoginActionTimeout { action, timeout } => {
                format!("Login action ({action}) timed out after {timeout:?}; shutting down.")
            }
            VmOutput::LoginActionFailed { action, status } => {
                format!("Login action ({action}) failed with exit code {status}")
            }
            VmOutput::LoginFailed(failure) => login_error(failure),
            VmOutput::LoginReady => {
                login_ready = true;
                return None;
            }
            VmOutput::ActionsCompleted => {
                actions_completed = true;
                if let Some(notice) = actions_completed_notice {
                    // Use stderr: the stdout forwarding thread holds the stdout
                    // lock for the whole run. The terminal may be in raw mode,
                    // so start on a fresh line.
                    eprintln!("\r\n{notice}\r");
                }
                return None;
            }
        };
        Some(error)
    };

    let mut exit_error = None;

    let final_state = loop {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.2),
            )
        };

        while exit_error.is_none()
            && let Ok(output) = vm_output_rx.try_recv()
        {
            exit_error = handle_output(output);
        }
        if exit_error.is_some() {
            request_stop(&vm);
            break None;
        }

        let state = unsafe { vm.state() };
        if state != VZVirtualMachineState::Running {
            break Some(state);
        }
    };

    // Wake any action still waiting for output, so the thread can be joined
    // right away instead of after its timeout.
    output_monitor.close();
    let _ = login_actions_thread.join();
    while exit_error.is_none()
        && let Ok(output) = vm_output_rx.try_recv()
    {
        exit_error = handle_output(output);
    }

    io_ctx.shutdown();

    let error = exit_error.or_else(|| (!login_ready).then(|| login_error(LoginFailure::VmStopped)));
    if let Some(error) = error {
        // Guest output usually ends mid-line; start the error on a new line.
        eprintln!();
        return Err(error.into());
    }

    Ok(RunOutcome {
        actions_completed,
        stopped_normally: final_state == Some(VZVirtualMachineState::Stopped),
        output: output_monitor.snapshot(),
    })
}
