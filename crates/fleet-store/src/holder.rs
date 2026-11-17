//! The lease holders Fleet names after a process of this machine:
//! `fleet-up-<pid>@<boot id>` and `fleet-<pid>@<boot id>`. The file store
//! lives on this machine, so a holder so named from another boot is
//! provably dead, with every process it started. A holder gone on this
//! boot is dead once the process groups it recorded are stopped.

use std::fs;
use std::path::{Path, PathBuf};

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
    parse(holder).is_some()
}

/// `true` only when the holder is proven dead with every process it
/// started: it names another boot of this machine, or it names a process
/// of this boot that is gone and whose record directory under `root`
/// ([`children_dir`]) is empty. A holder of any other shape, a process
/// that left no record directory, or a check the platform cannot make, is
/// never dead.
pub fn is_dead(holder: &str, root: &Path) -> bool {
    match judge(holder) {
        Judged::OtherBoot => true,
        Judged::GoneThisBoot => children_dir(root, holder)
            .and_then(|dir| fs::read_dir(dir).ok())
            .is_some_and(|mut entries| entries.next().is_none()),
        Judged::Undecided => false,
    }
}

/// Whether the next manager may take the lease from `holder`: it is dead,
/// or it is gone on this boot and left a record directory, whose groups the
/// next manager stops first.
pub fn is_takeable(holder: &str, root: &Path) -> bool {
    match judge(holder) {
        Judged::OtherBoot => true,
        Judged::GoneThisBoot => children_dir(root, holder).is_some_and(|dir| dir.is_dir()),
        Judged::Undecided => false,
    }
}

/// Where the holder named after a process records the process groups it
/// starts: `<root>/children/<pid>@<boot id>`.
pub fn children_dir(root: &Path, holder: &str) -> Option<PathBuf> {
    let (pid, boot) = parse(holder)?;
    Some(root.join(CHILDREN).join(format!("{pid}@{boot}")))
}

/// A record directory under `<root>/children` whose process is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftBehind {
    pub dir: PathBuf,
    /// Its process ran on this boot: its groups may still run.
    pub this_boot: bool,
    /// It names the holder of the lease, whose taker judges it by it.
    pub holds_lease: bool,
}

/// Every record directory whose process is gone; `lease_holder` is the
/// holder `lease.json` names.
pub fn left_behind(root: &Path, lease_holder: Option<&str>) -> Vec<LeftBehind> {
    let Some(current) = boot_id() else {
        return Vec::new();
    };
    let held = lease_holder.and_then(|h| children_dir(root, h));
    let Ok(entries) = fs::read_dir(root.join(CHILDREN)) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let dir = entry.ok()?.path();
            let (pid, boot) = dir.file_name()?.to_str()?.split_once('@')?;
            let this_boot = boot == current;
            if this_boot && alive(pid) {
                return None;
            }
            Some(LeftBehind {
                holds_lease: held.as_ref() == Some(&dir),
                dir,
                this_boot,
            })
        })
        .collect()
}

/// Remove a record directory and the records in it.
pub fn forget(dir: &Path) -> std::io::Result<()> {
    fs::remove_dir_all(dir)
}

pub const CHILDREN: &str = "children";

enum Judged {
    OtherBoot,
    GoneThisBoot,
    Undecided,
}

fn judge(holder: &str) -> Judged {
    match (parse(holder), boot_id()) {
        (Some((_, boot)), Some(current)) if boot != current => Judged::OtherBoot,
        (Some((pid, _)), Some(_)) if !alive(pid) => Judged::GoneThisBoot,
        _ => Judged::Undecided,
    }
}

fn parse(holder: &str) -> Option<(&str, &str)> {
    let (process, boot) = holder.rsplit_once('@')?;
    let pid = ["fleet-up-", "fleet-"]
        .iter()
        .find_map(|prefix| process.strip_prefix(prefix))?;
    let named = !boot.is_empty() && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit());
    named.then_some((pid, boot))
}

fn alive(pid: &str) -> bool {
    Path::new("/proc").join(pid).exists()
}

/// This machine's current boot id; `None` where the platform gives none.
#[cfg(target_os = "linux")]
pub fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

#[cfg(not(target_os = "linux"))]
pub fn boot_id() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nowhere() -> &'static Path {
        Path::new("/nonexistent-fleet-store")
    }

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
            assert!(!is_dead(holder, nowhere()), "{holder}");
            assert!(!is_takeable(holder, nowhere()), "{holder}");
            assert!(!is_reserved(holder), "{holder}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_calling_process_is_alive_and_another_boot_is_dead() {
        let me = local(true);
        assert!(is_reserved(&me), "{me}");
        assert!(!is_dead(&me, nowhere()), "{me}");
        let (process, _) = me.rsplit_once('@').unwrap();
        assert!(is_dead(&format!("{process}@another-boot"), nowhere()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_gone_on_this_boot_is_dead_once_its_records_are_gone() {
        let root = std::env::temp_dir().join(format!("fleet-holder-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        let holder = format!("fleet-up-{pid}@{}", boot_id().unwrap());
        assert!(!is_takeable(&holder, &root), "no record directory");
        assert!(!is_dead(&holder, &root), "no record directory");

        let dir = children_dir(&root, &holder).unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("0"), "1 1").unwrap();
        assert!(is_takeable(&holder, &root));
        assert!(!is_dead(&holder, &root), "a recorded group may still run");
        let left = left_behind(&root, Some(&holder));
        assert_eq!(
            left,
            vec![LeftBehind {
                dir: dir.clone(),
                this_boot: true,
                holds_lease: true
            }]
        );

        fs::remove_file(dir.join("0")).unwrap();
        assert!(is_dead(&holder, &root));
        forget(&dir).unwrap();
        assert!(left_behind(&root, None).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
