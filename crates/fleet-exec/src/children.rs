//! The record of the process groups a lease holder's [`crate::run`] calls
//! lead, so that a process taking over the lease of a holder that died on
//! this boot stops them first. One file per run in the directory
//! [`record_in`] names: written empty before the spawn, then
//! `<pgid> <leader start time>` once the group exists, and removed when the
//! run returns. An empty file is a run whose group cannot be known.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static DIR: OnceLock<PathBuf> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(0);

/// How long [`stop_recorded`] waits for a killed group to be gone.
pub const STOP_BOUND: Duration = Duration::from_secs(10);

/// Record every later run of this process in `dir`, which is created now.
/// The first directory named wins.
pub fn record_in(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let _ = DIR.set(dir.to_path_buf());
    Ok(())
}

pub(crate) struct Record(PathBuf);

impl Record {
    pub(crate) fn intend() -> io::Result<Option<Record>> {
        let Some(dir) = DIR.get() else {
            return Ok(None);
        };
        let path = dir.join(SEQ.fetch_add(1, Ordering::Relaxed).to_string());
        fs::write(&path, b"")?;
        Ok(Some(Record(path)))
    }

    pub(crate) fn started(&self, pgid: u32) -> io::Result<()> {
        let start = start_time(pgid).unwrap_or_default();
        fs::write(&self.0, format!("{pgid} {start}"))
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Kill every group recorded in `dir`, wait until each is gone, and remove
/// its record. An empty record, a group that outlives [`STOP_BOUND`] or a
/// platform with no `/proc` is an error, and leaves its record in place.
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
    if start_time(pgid).is_some_and(|now| now != start) {
        return Ok(());
    }
    let deadline = Instant::now() + STOP_BOUND;
    while group_alive(pgid)? {
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "process group {pgid} outlived its kill"
            )));
        }
        let _ = std::process::Command::new("kill")
            .args(["-s", "KILL", "--", &format!("-{pgid}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn stat_fields_after_command_name(pid: &str) -> Option<Vec<String>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    Some(rest.split_whitespace().map(str::to_string).collect())
}

fn start_time(pid: u32) -> Option<u64> {
    stat_fields_after_command_name(&pid.to_string())?
        .get(19)?
        .parse()
        .ok()
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
        if let Some(fields) = stat_fields_after_command_name(pid)
            && fields.first().map(String::as_str) != Some("Z")
            && fields.get(2) == Some(&pgid)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
