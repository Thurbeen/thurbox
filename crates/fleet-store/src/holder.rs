//! The lease holders Fleet names after a process of this machine:
//! `fleet-up-<pid>@<boot id>` and `fleet-<pid>@<boot id>`. The file store
//! lives on this machine, so a holder so named from another boot is
//! provably dead, with every process it started.

/// The holder name for the calling process: `fleet-up-<pid>@<boot id>`
/// for `fleet up`, `fleet-<pid>@<boot id>` otherwise. Without a boot id
/// the name has no `@` part and is never judged dead.
pub fn local(serving: bool) -> String {
    let prefix = if serving { "fleet-up" } else { "fleet" };
    let pid = std::process::id();
    match boot_id() {
        Some(boot) => format!("{prefix}-{pid}@{boot}"),
        None => format!("{prefix}-{pid}"),
    }
}

/// Whether `holder` has the shape [`local`] gives, which a holder chosen
/// by hand must not take.
pub fn is_reserved(holder: &str) -> bool {
    boot_of(holder).is_some()
}

/// `true` only when the holder is proven dead: it names another boot of
/// this machine, so neither it nor any process it started still runs. A
/// holder of any other shape, or a check the platform cannot make, is
/// never dead. A dead process of this boot is not enough: the processes
/// it started outlive it, unbounded.
pub fn is_dead(holder: &str) -> bool {
    match (boot_of(holder), boot_id()) {
        (Some(boot), Some(current)) => boot != current,
        _ => false,
    }
}

fn boot_of(holder: &str) -> Option<&str> {
    let (process, boot) = holder.rsplit_once('@')?;
    let pid = ["fleet-up-", "fleet-"]
        .iter()
        .find_map(|prefix| process.strip_prefix(prefix))?;
    let named = !boot.is_empty() && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit());
    named.then_some(boot)
}

#[cfg(target_os = "linux")]
fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn boot_id() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_holder_not_named_after_a_process_is_never_dead() {
        for holder in [
            "ops",
            "fleet-up",
            "fleet-up-x@b",
            "fleet-up-1@",
            "fleet-up-1",
            "ci-1@b",
        ] {
            assert!(!is_dead(holder), "{holder}");
            assert!(!is_reserved(holder), "{holder}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_calling_process_is_alive_and_another_boot_is_dead() {
        let me = local(true);
        assert!(is_reserved(&me), "{me}");
        assert!(!is_dead(&me), "{me}");
        let (process, _) = me.rsplit_once('@').unwrap();
        assert!(is_dead(&format!("{process}@another-boot")));
    }
}
