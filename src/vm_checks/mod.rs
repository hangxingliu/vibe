//! VM checks for the `#[ignore]` integration tests.
//!
//! Virtualization.framework completion handlers need the process main thread,
//! and `cargo test` threads are not the main thread. The tests therefore spawn
//! the `vibe` binary with `VIBE_VM_CHECK=<name>`, and `main()` calls [`run`]
//! before parsing the CLI. The normal CLI never sets this variable.
//!
//! - `read-only-disk`, `headless-timeout`: [`devices`]
//! - `maintenance-<name>` (for example `maintenance-all`): [`maintenance`]
//! - `probe`, `prepare`: [`prepare`]

mod devices;
mod maintenance;
mod prepare;

pub(crate) fn run(which: &str) -> Result<(), Box<dyn std::error::Error>> {
    match which {
        "read-only-disk" => devices::run_read_only_disk(),
        "headless-timeout" => devices::run_headless_timeout(),
        "probe" => prepare::check_probe(),
        "prepare" => prepare::check_prepare(),
        other => match other.strip_prefix("maintenance-") {
            Some(name) => maintenance::run(name),
            None => Err(format!("unknown VIBE_VM_CHECK value '{other}'").into()),
        },
    }
}
