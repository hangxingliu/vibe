//! VM lifecycle helpers shared by VM runners: start a configured VM and wait
//! until it is running, and request it to stop.

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{AnyThread, rc::Retained};
use objc2_foundation::*;
use objc2_virtualization::*;

const START_TIMEOUT: Duration = Duration::from_secs(60);

/// Creates a VM from `config` on the main dispatch queue, starts it, and runs
/// the main run loop until the start completion handler fires.
pub(crate) fn start_vm(
    config: &VZVirtualMachineConfiguration,
) -> Result<Retained<VZVirtualMachine>, Box<dyn std::error::Error>> {
    let queue = DispatchQueue::main();

    let vm = unsafe {
        VZVirtualMachine::initWithConfiguration_queue(VZVirtualMachine::alloc(), config, queue)
    };

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let completion_handler = RcBlock::new(move |error: *mut NSError| {
        if error.is_null() {
            let _ = tx.send(Ok(()));
        } else {
            let err = unsafe { &*error };
            let _ = tx.send(Err(format!("{:?}", err.localizedDescription())));
        }
    });

    unsafe {
        vm.startWithCompletionHandler(&completion_handler);
    }

    let start_deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < start_deadline {
        unsafe {
            NSRunLoop::mainRunLoop().runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.1),
            )
        };

        match rx.try_recv() {
            Ok(result) => {
                result.map_err(|e| format!("Failed to start VM: {e}"))?;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("VM start channel disconnected".into());
            }
        }
    }

    if Instant::now() >= start_deadline {
        return Err("Timed out waiting for VM to start".into());
    }

    Ok(vm)
}

/// Asks the guest to stop; falls back to a hard stop when a request is not possible.
pub(crate) fn request_stop(vm: &VZVirtualMachine) {
    unsafe {
        if vm.canRequestStop() {
            if let Err(err) = vm.requestStopWithError() {
                eprintln!("Failed to request VM stop: {err:?}");
            }
        } else if vm.canStop() {
            let handler = RcBlock::new(|_error: *mut NSError| {});
            vm.stopWithCompletionHandler(&handler);
        }
    }
}
