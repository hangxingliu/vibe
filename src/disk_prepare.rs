//! Decide whether a provision work copy needs the Alpine maintenance VM.
//!
//! Only `provision_image` calls this, and only on the private `name.tmp.<pid>`
//! copy. The run path does not. `LegacyCompatible` and `Unsupported` never
//! touch the network. `process_disk` takes the work-copy lock itself; taking
//! another flock here would block that acquire in this same process.

use std::{fs, path::Path};

use crate::{
    alpine::ensure_alpine_image,
    cache::disk_lock_path,
    image_probe::{ImageClass, MachineIdFact, ProbeFacts, probe_disk},
    maintenance::process_disk,
};

/// What `ensure_bootable` did. Callers use this for messages and tests.
/// Boot is legacy either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedKind {
    AlreadyCompatible,
    Unsupported,
    Processed,
}

impl std::fmt::Display for PreparedKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreparedKind::AlreadyCompatible => write!(f, "AlreadyCompatible"),
            PreparedKind::Unsupported => write!(f, "Unsupported"),
            PreparedKind::Processed => write!(f, "Processed"),
        }
    }
}

/// Pure mapping from [`ImageClass`] to the prepare step. `classify` stays
/// free of this policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrepareDecision {
    AlreadyCompatible,
    WarnUnsupported,
    Process,
}

pub(crate) fn prepare_decision(class: &ImageClass) -> PrepareDecision {
    match class {
        ImageClass::LegacyCompatible => PrepareDecision::AlreadyCompatible,
        ImageClass::Unsupported(_) => PrepareDecision::WarnUnsupported,
        ImageClass::NeedsPreparation | ImageClass::Unknown(_) => PrepareDecision::Process,
    }
}

const EMPTY_ROOT_NOTICE: &str = "After preparation, root has an empty password in this image and in VMs created from it (the same as the older nocloud image).";

/// User-facing reason for a one-time preparation. Never includes the password field.
pub(crate) fn preparation_notice(class: &ImageClass, facts: &ProbeFacts) -> String {
    match class {
        ImageClass::Unknown(reason) => format!(
            "Could not classify this disk image ({reason}). Preparing it with the Alpine maintenance VM, which checks the image itself. This takes about 20 seconds."
        ),
        ImageClass::NeedsPreparation => format!(
            "Detected a Debian image that needs a one-time preparation ({}). Preparing it with the Alpine maintenance VM. This takes about 20 seconds.",
            needs_preparation_reasons(facts).join("; ")
        ),
        ImageClass::LegacyCompatible | ImageClass::Unsupported(_) => String::new(),
    }
}

/// Warning printed when the probe finds an image Vibe will not prepare.
pub(crate) fn unsupported_warning(class: &ImageClass) -> String {
    format!(
        "[probe] Warning: this disk is {class}. Vibe will not prepare it and will boot it on the existing legacy path."
    )
}

fn needs_preparation_reasons(facts: &ProbeFacts) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    match facts.machine_id {
        MachineIdFact::Uninitialized => reasons.push("machine-id is uninitialized"),
        MachineIdFact::Other => reasons.push("machine-id is not empty or a valid id"),
        MachineIdFact::Missing
        | MachineIdFact::EmptyFile
        | MachineIdFact::Valid
        | MachineIdFact::Symlink(_) => {}
    }
    if facts.root_password_empty == Some(false) {
        reasons.push("the root password field is not empty");
    }
    if reasons.is_empty() {
        reasons.push("the image is not legacy compatible");
    }
    reasons
}

/// Probe `work_copy` and, when needed, prepare it in place with Alpine.
/// `cache_dir` holds the Alpine image and the maintenance work directories.
pub(crate) fn ensure_bootable(
    work_copy: &Path,
    cache_dir: &Path,
) -> Result<PreparedKind, Box<dyn std::error::Error>> {
    let (class, facts) = probe_disk(work_copy);
    match prepare_decision(&class) {
        PrepareDecision::AlreadyCompatible => {
            println!("Disk is already legacy compatible; skipping Alpine preparation.");
            Ok(PreparedKind::AlreadyCompatible)
        }
        PrepareDecision::WarnUnsupported => {
            eprintln!("{}", unsupported_warning(&class));
            Ok(PreparedKind::Unsupported)
        }
        PrepareDecision::Process => {
            println!("{}", preparation_notice(&class, &facts));
            println!("{EMPTY_ROOT_NOTICE}");
            let alpine = ensure_alpine_image(cache_dir)?;
            let result = process_disk(work_copy, &alpine, cache_dir);
            // The sibling lock file is only ours when this call was not rejected
            // because another process already holds it. Unlinking a lock that
            // someone else holds would let a third process lock a new inode.
            let in_use = matches!(
                &result,
                Err(err) if err.detail.contains("in use by another Vibe process")
            );
            if !in_use {
                let _ = fs::remove_file(disk_lock_path(work_copy));
            }
            result?;
            println!("Preparation finished.");
            Ok(PreparedKind::Processed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_probe::{ImageClass, MachineIdFact, ProbeFacts};

    fn facts_with(machine_id: MachineIdFact, root_password_empty: Option<bool>) -> ProbeFacts {
        ProbeFacts {
            machine_id,
            root_password_empty,
            ..ProbeFacts::default()
        }
    }

    #[test]
    fn prepare_decision_maps_every_image_class() {
        assert_eq!(
            prepare_decision(&ImageClass::LegacyCompatible),
            PrepareDecision::AlreadyCompatible
        );
        assert_eq!(
            prepare_decision(&ImageClass::Unsupported("no GPT root".into())),
            PrepareDecision::WarnUnsupported
        );
        assert_eq!(
            prepare_decision(&ImageClass::Unsupported("not Debian".into())),
            PrepareDecision::WarnUnsupported
        );
        assert_eq!(
            prepare_decision(&ImageClass::NeedsPreparation),
            PrepareDecision::Process
        );
        assert_eq!(
            prepare_decision(&ImageClass::Unknown("needs_recovery".into())),
            PrepareDecision::Process
        );
    }

    #[test]
    fn preparation_notice_describes_state_without_the_password_field() {
        let facts = facts_with(MachineIdFact::Uninitialized, Some(false));
        let notice = preparation_notice(&ImageClass::NeedsPreparation, &facts);
        assert!(notice.contains("machine-id is uninitialized"));
        assert!(notice.contains("the root password field is not empty"));
        assert!(notice.contains("about 20 seconds"));
        assert!(!notice.contains("unprovisioned"));
        assert!(!notice.contains("$y$"));
        assert!(!notice.contains('!'));

        let unknown = preparation_notice(
            &ImageClass::Unknown("needs_recovery".into()),
            &facts_with(MachineIdFact::Missing, None),
        );
        assert!(unknown.contains("needs_recovery"));
        assert!(unknown.contains("Alpine maintenance VM"));
        assert!(!unknown.contains("unprovisioned"));

        let other = preparation_notice(
            &ImageClass::NeedsPreparation,
            &facts_with(MachineIdFact::Other, Some(true)),
        );
        assert!(other.contains("machine-id is not empty or a valid id"));
        assert!(!other.contains("the root password field is not empty"));
    }

    #[test]
    fn unsupported_warning_uses_the_probe_prefix() {
        let warning = unsupported_warning(&ImageClass::Unsupported("not Debian".into()));
        assert!(warning.starts_with("[probe] Warning: this disk is unsupported: not Debian."));
    }

    #[test]
    fn empty_root_notice_does_not_include_a_password_value() {
        assert!(EMPTY_ROOT_NOTICE.contains("empty password"));
        assert!(!EMPTY_ROOT_NOTICE.contains("unprovisioned"));
    }
}
