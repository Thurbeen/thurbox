//! Lease holders named after a process of this machine: `<prefix>-<pid>`,
//! with `@<boot id>` where the platform has one. The file store lives on
//! this machine, so a holder so named can be proven dead.

use std::fs;
use std::path::Path;

/// The holder name for the calling process: `<prefix>-<pid>@<boot id>`.
pub fn local(prefix: &str) -> String {
    let pid = std::process::id();
    match boot_id() {
        Some(boot) => format!("{prefix}-{pid}@{boot}"),
        None => format!("{prefix}-{pid}"),
    }
}

/// `true` only when the holder is proven dead: it names another boot of
/// this machine, or a process that does not exist. A holder of any other
/// shape, or a check the platform cannot make, is never dead.
pub fn is_dead(holder: &str) -> bool {
    let (process, boot) = match holder.rsplit_once('@') {
        Some((_, "")) => return false,
        Some((process, boot)) => (process, Some(boot)),
        None => (holder, None),
    };
    let Some(pid) = process
        .rsplit_once('-')
        .and_then(|(_, pid)| pid.parse::<u32>().ok())
    else {
        return false;
    };
    let Some(current) = boot_id() else {
        return false;
    };
    match boot {
        Some(boot) if boot != current => true,
        _ => !process_exists(pid),
    }
}

#[cfg(target_os = "linux")]
fn boot_id() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn boot_id() -> Option<String> {
    None
}

fn process_exists(pid: u32) -> bool {
    !matches!(
        fs::metadata(Path::new("/proc").join(pid.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_holder_not_named_after_a_process_is_never_dead() {
        for holder in ["ops", "fleet-up", "fleet-up-x", "fleet-up-1@"] {
            assert!(!is_dead(holder), "{holder}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_calling_process_is_alive_and_another_boot_is_dead() {
        let me = local("fleet-up");
        assert!(!is_dead(&me), "{me}");
        let (process, _) = me.rsplit_once('@').unwrap();
        assert!(is_dead(&format!("{process}@another-boot")));
    }
}
