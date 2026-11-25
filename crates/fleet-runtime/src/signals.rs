use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fleet_api::Observation;
use fleet_api::event::{OutboxRead, outbox_file};
use fleet_api::signal::{MAX_LINE, Reported, Signal, Signals};
use fleet_api::task::{LOCAL_HOST, SessionRef};

const MAX_FILE: u64 = 8 * 1024 * 1024;

pub const PLACE_DIR: &str = "~/.local/state/fleet/signals";

pub fn place_file(key: &str) -> Option<String> {
    let plain = !key.is_empty() && !key.starts_with('.') && !key.contains(['/', '\\']);
    plain.then(|| format!("{PLACE_DIR}/{key}.jsonl"))
}

pub const RESTART_SPLIT: &str = "@@fleet-restart@@";

pub const PLACE_READ_BEFORE: u64 = 256 * 1024;
pub const PLACE_READ_AFTER: u64 = 512 * 1024;

pub fn read_split(file: &Path, since: Option<u64>) -> io::Result<Option<Vec<u8>>> {
    let mut f = match crate::events::open_regular(&expand_home(file)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        opened => opened?,
    };
    let len = f.metadata()?.len();
    let since = since.filter(|&o| o <= len).unwrap_or(0);
    let mut range = |from: u64, count: u64, out: &mut Vec<u8>| -> io::Result<()> {
        f.seek(SeekFrom::Start(from))?;
        (&mut f).take(count).read_to_end(out).map(drop)
    };
    let mut out = Vec::new();
    let start = since.saturating_sub(PLACE_READ_BEFORE);
    range(start, since - start, &mut out)?;
    out.extend_from_slice(format!("\n{RESTART_SPLIT}\n").as_bytes());
    let after = (len - since).min(PLACE_READ_AFTER);
    range(len - after, after, &mut out)?;
    Ok(Some(out))
}

pub fn file_length(file: &Path) -> io::Result<u64> {
    match fs::metadata(expand_home(file)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        meta => Ok(meta?.len()),
    }
}

pub fn expand_home(file: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match (file.to_string_lossy().strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => file.to_path_buf(),
    }
}

#[derive(Debug, Clone)]
pub struct FileSignals {
    dir: PathBuf,
}

impl FileSignals {
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<FileSignals> {
        let dir = dir.into();
        crate::events::create_private_dir_all(&dir)?;
        Ok(FileSignals { dir })
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        let plain = !key.is_empty() && !key.starts_with('.') && !key.contains(['/', '\\']);
        plain.then(|| self.dir.join(format!("{key}.jsonl")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    At(u64),
    Unknown,
}

const UNKNOWN_BOUNDARY: &str = "unknown";

impl FileSignals {
    pub fn mark_started(&self, key: &str, boundary: Boundary) -> io::Result<()> {
        let path = self
            .started_path(key)
            .ok_or_else(|| io::Error::other("the session key is not a plain name"))?;
        let tmp = path.with_extension("started.tmp");
        let text = match boundary {
            Boundary::At(offset) => offset.to_string(),
            Boundary::Unknown => UNKNOWN_BOUNDARY.to_string(),
        };
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &path)
    }

    pub fn started(&self, key: &str) -> Option<Boundary> {
        let path = self.started_path(key)?;
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(_) => return Some(Boundary::Unknown),
        };
        Some(text.trim().parse().map_or(Boundary::Unknown, Boundary::At))
    }

    fn started_path(&self, key: &str) -> Option<PathBuf> {
        self.path(key).map(|p| p.with_extension("started"))
    }
}

pub fn append(file: &Path, signal: &Signal) -> io::Result<()> {
    let mut opened = OpenOptions::new().create(true).append(true).open(file)?;
    let placed = Signal {
        seq: Some(opened.metadata()?.len() + 1),
        ..signal.clone()
    };
    let mut line = serde_json::to_vec(&placed).map_err(io::Error::other)?;
    line.push(b'\n');
    if line.len() > MAX_LINE {
        return Err(io::Error::other("the signal is too long"));
    }
    opened.write_all(&line)
}

impl Signals for FileSignals {
    fn file(&self, host: &str, key: &str) -> Option<String> {
        if host != LOCAL_HOST {
            return place_file(key);
        }
        self.path(key).map(|p| p.to_string_lossy().into_owned())
    }

    fn outbox(&self, session: &SessionRef, from: u64) -> Observation<Option<OutboxRead>> {
        if session.host != LOCAL_HOST {
            return Observation::unknown(format!(
                "the outbox on host {} is read through the runtime's exec, which this runtime does not offer",
                session.host
            ));
        }
        let Some(path) = self.path(&session.key) else {
            return Observation::unknown("the session key is not a plain name");
        };
        let file = outbox_file(&path.to_string_lossy());
        match crate::events::read_from(Path::new(&file), from) {
            Ok(read) => Observation::Known(read),
            Err(e) => Observation::unknown(format!("cannot read the outbox: {e}")),
        }
    }

    fn written(&self, session: &SessionRef) -> Option<u64> {
        if session.host != LOCAL_HOST {
            return None;
        }
        match fs::metadata(self.path(&session.key)?) {
            Ok(meta) => Some(meta.len()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Some(0),
            Err(_) => None,
        }
    }

    fn read(&self, session: &SessionRef) -> Observation<Reported> {
        if session.host != LOCAL_HOST {
            return Observation::unknown(format!(
                "signals from host {} are read through the runtime's exec, which this runtime does not offer",
                session.host
            ));
        }
        let Some(path) = self.path(&session.key) else {
            return Observation::unknown("the session key is not a plain name");
        };
        let read = || -> io::Result<String> {
            let mut file = crate::events::open_regular(&path)?;
            let len = file.metadata()?.len();
            file.seek(SeekFrom::Start(len.saturating_sub(MAX_FILE)))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        };
        match read() {
            Ok(text) => Observation::Known(Reported::fold(&text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Observation::Known(Reported::default())
            }
            Err(e) => Observation::unknown(format!("cannot read the signal file: {e}")),
        }
    }
}

#[cfg(test)]
mod place_read_tests {
    use super::*;
    use fleet_api::signal::State;

    #[test]
    fn a_place_read_returns_the_two_ranges_around_the_restart() {
        let dir = std::env::temp_dir().join(format!("fleet-place-read-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("k.jsonl");
        let mut f = fs::File::create(&path).expect("file");
        let half: u64 = 32 * 1024 * 1024;
        f.write_all(b"{\"at\":1,\"state\":\"idle\"}\n")
            .expect("old");
        f.seek(SeekFrom::Start(half)).expect("seek");
        f.write_all(b"\n").expect("line");
        let offset = f.stream_position().expect("offset");
        f.seek(SeekFrom::Start(2 * half)).expect("seek");
        f.write_all(b"\n{\"at\":2,\"state\":\"working\"}\n")
            .expect("new");
        let size = f.stream_position().expect("size");
        drop(f);
        assert_eq!(file_length(&path).expect("length"), size);
        let out = read_split(&path, Some(offset))
            .expect("read")
            .expect("there");
        assert!(out.len() < 1024 * 1024, "{} bytes", out.len());
        let text = String::from_utf8_lossy(&out);
        let (before, after) = text.split_once(RESTART_SPLIT).expect("the split");
        assert_eq!(
            Reported::fold_restarted(before, after).state,
            Some(State::Working)
        );
        let out = read_split(&path, Some(size + 1))
            .expect("read")
            .expect("there");
        assert!(String::from_utf8_lossy(&out).starts_with(&format!("\n{RESTART_SPLIT}\n")));
        let gone = dir.join("none.jsonl");
        assert_eq!(read_split(&gone, None).expect("read"), None);
        assert_eq!(file_length(&gone).expect("length"), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_or_a_symlink_at_a_signal_file_is_unknown_at_once() {
        let dir = std::env::temp_dir().join(format!("fleet-signal-fifo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let signals = FileSignals::open(&dir).expect("signals");
        let fifo = dir.join("k.jsonl");
        let made = fleet_exec::run(
            &fleet_exec::Spec::new("mkfifo", std::time::Duration::from_secs(5)).arg(&fifo),
        );
        assert!(
            matches!(made, fleet_exec::Outcome::Exited { code: Some(0), .. }),
            "{made:?}"
        );
        let session = SessionRef {
            id: "s".into(),
            host: LOCAL_HOST.into(),
            key: "k".into(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let (path, read_signals, read_session) = (fifo.clone(), signals.clone(), session.clone());
        std::thread::spawn(move || {
            let folded = read_signals.read(&read_session).is_unknown();
            let split = read_split(&path, None).is_err();
            tx.send((folded, split)).ok();
        });
        let (folded, split) = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the reads return instead of blocking");
        assert!(folded, "a FIFO folds to Unknown");
        assert!(split, "a place read refuses a FIFO");

        fs::remove_file(&fifo).expect("fifo");
        let secret = dir.join("secret");
        fs::write(&secret, "{\"at\":1,\"state\":\"idle\"}\n").expect("secret");
        std::os::unix::fs::symlink(&secret, &fifo).expect("symlink");
        assert!(
            signals.read(&session).is_unknown(),
            "a symlink is not followed"
        );
        assert!(read_split(&fifo, None).is_err(), "nor on a place");
        let _ = fs::remove_dir_all(&dir);
    }
}
