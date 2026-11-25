use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static DIR: OnceLock<PathBuf> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

pub const STOP_BOUND: Duration = Duration::from_secs(10);

pub fn record_in(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let _ = DIR.set(dir.to_path_buf());
    Ok(())
}

pub(crate) struct Record(PathBuf);

impl Record {
    pub(crate) fn intend() -> io::Result<Option<Record>> {
        DIR.get().map(|dir| Record::intend_in(dir)).transpose()
    }

    fn intend_in(dir: &Path) -> io::Result<Record> {
        loop {
            let path = dir.join(SEQ.fetch_add(1, Ordering::Relaxed).to_string());
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Record(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub(crate) fn started(&self, pgid: u32) -> io::Result<()> {
        let start =
            start_time(pgid)?.ok_or_else(|| io::Error::other(format!("process {pgid} is gone")))?;
        fs::write(&self.0, format!("{pgid} {start}"))
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn stop_recorded(dir: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let (pgid, start) = text
            .split_once(' ')
            .and_then(|(g, s)| Some((g.parse::<u32>().ok()?, s.parse::<u64>().ok()?)))
            .ok_or_else(|| {
                io::Error::other(format!("{}: a run whose group is unknown", path.display()))
            })?;
        stop_group(pgid, start)?;
        match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

fn stop_group(pgid: u32, start: u64) -> io::Result<()> {
    if !Path::new("/proc/self/stat").exists() {
        return Err(io::Error::other("no /proc: the group cannot be checked"));
    }
    if start_time(pgid)?.is_some_and(|now| now != start) {
        return Ok(());
    }
    let deadline = Instant::now() + STOP_BOUND;
    while group_alive(pgid)? {
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "process group {pgid} outlived its kill"
            )));
        }
        crate::run(&crate::Spec::new("kill", KILL_BOUND).args([
            "-s",
            "KILL",
            "--",
            &format!("-{pgid}"),
        ]));
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

const KILL_BOUND: Duration = Duration::from_secs(2);
const ESRCH: i32 = 3;

fn stat_fields_after_command_name(pid: &str) -> io::Result<Option<Vec<String>>> {
    let path = format!("/proc/{pid}/stat");
    let stat = match fs::read(&path) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(ESRCH) => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let after_name = stat
        .iter()
        .rposition(|&b| b == b')')
        .ok_or_else(|| io::Error::other(format!("{path}: no command name")))?;
    Ok(Some(
        String::from_utf8_lossy(&stat[after_name + 1..])
            .split_whitespace()
            .map(str::to_string)
            .collect(),
    ))
}

fn start_time(pid: u32) -> io::Result<Option<u64>> {
    let Some(fields) = stat_fields_after_command_name(&pid.to_string())? else {
        return Ok(None);
    };
    fields
        .get(19)
        .and_then(|start| start.parse().ok())
        .map(Some)
        .ok_or_else(|| io::Error::other(format!("/proc/{pid}/stat: no start time")))
}

fn group_alive(pgid: u32) -> io::Result<bool> {
    let pgid = pgid.to_string();
    for entry in fs::read_dir("/proc")? {
        let name = entry?.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        if let Some(fields) = stat_fields_after_command_name(pid)?
            && fields.first().map(String::as_str) != Some("Z")
            && fields.get(2) == Some(&pgid)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    #[test]
    fn a_group_whose_name_is_cut_mid_character_is_still_stopped() {
        let root = std::env::temp_dir().join(format!("fleet-children-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("records")).unwrap();
        let sleep = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|d| d.join("sleep"))
            .find(|p| p.is_file())
            .unwrap();
        let mut name = b"aaaaaaaaaaaaaa".to_vec();
        name.extend_from_slice("é".as_bytes());
        let cut_name = root.join(std::ffi::OsStr::from_bytes(&name));
        std::os::unix::fs::symlink(&sleep, &cut_name).unwrap();
        let mut child = std::process::Command::new(&cut_name)
            .arg0("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        let start = loop {
            if let Ok(Some(start)) = start_time(pgid) {
                break start;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the start time of a running process could not be read");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        fs::write(root.join("records/0"), format!("{pgid} {start}")).unwrap();

        stop_recorded(&root.join("records")).unwrap();

        let stopped = child.try_wait().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_dir_all(&root);
        assert!(stopped.is_some(), "the group was left running");
    }

    #[test]
    fn a_record_left_by_an_earlier_process_with_this_pid_is_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("fleet-reuse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("0"), "4242 7").unwrap();

        let record = Record::intend_in(&dir).unwrap();
        record.started(std::process::id()).unwrap();
        let left = fs::read_to_string(dir.join("0")).unwrap();
        drop(record);

        assert_eq!(left, "4242 7");
        assert!(dir.join("0").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_caught_before_its_group_was_recorded_is_never_stopped_as_done() {
        let dir = std::env::temp_dir().join(format!("fleet-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("0"), "").unwrap();

        let stopped = stop_recorded(&dir);

        let kept = dir.join("0").exists();
        let _ = fs::remove_dir_all(&dir);
        assert!(
            stopped
                .as_ref()
                .is_err_and(|e| e.to_string().contains("a run whose group is unknown")),
            "{stopped:?}"
        );
        assert!(kept, "the record of a run whose group is unknown stays");
    }
}
