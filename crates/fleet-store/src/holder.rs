use std::fs;
use std::path::{Path, PathBuf};

pub fn local(serving: bool) -> String {
    let prefix = if serving { "fleet-up" } else { "fleet" };
    let pid = std::process::id();
    match boot_id() {
        Some(boot) => format!("{prefix}-{pid}@{boot}"),
        None => format!("{prefix}-{pid}"),
    }
}

pub fn is_reserved(holder: &str) -> bool {
    parse(holder).is_some()
}

pub fn is_dead(holder: &str, root: &Path) -> bool {
    match judge(holder) {
        Judged::OtherBoot => true,
        Judged::GoneThisBoot => children_dir(root, holder)
            .and_then(|dir| fs::read_dir(dir).ok())
            .is_some_and(|mut entries| entries.next().is_none()),
        Judged::Undecided => false,
    }
}

pub fn is_takeable(holder: &str, root: &Path) -> bool {
    match judge(holder) {
        Judged::OtherBoot => true,
        Judged::GoneThisBoot => children_dir(root, holder).is_some_and(|dir| dir.is_dir()),
        Judged::Undecided => false,
    }
}

pub fn children_dir(root: &Path, holder: &str) -> Option<PathBuf> {
    let (pid, boot) = parse(holder)?;
    Some(root.join(CHILDREN).join(format!("{pid}@{boot}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftBehind {
    pub dir: PathBuf,
    pub this_boot: bool,
    pub holds_lease: bool,
}

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
        let never_assigned_pid = fs::read_to_string("/proc/sys/kernel/pid_max").unwrap();
        let pid = never_assigned_pid.trim();
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
