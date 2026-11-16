//! The signal port's adapter: one file per session under Fleet's data
//! directory, on this machine.
//!
//! A session on the local host gets `<dir>/<key>.jsonl` and its hooks
//! append to it through `fleet signal`. A session on any other host gets a
//! file on that host, under its home ([`place_file`]), which the `fleet`
//! there appends to. This adapter cannot read it: the exec runtime reads it
//! back through the runtime's `exec`, over the connection the runtime
//! already has to the host.
//!
//! ```text
//! local:  hook ─fleet signal─▶ <data>/signals/<key>.jsonl ◀─read── Fleet
//! remote: hook ─fleet signal─▶ ~/.local/state/fleet/signals/<key>.jsonl
//!                                    ◀─runtime exec (tail)── Fleet
//! ```

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fleet_api::Observation;
use fleet_api::event::{OutboxRead, outbox_file};
use fleet_api::signal::{MAX_LINE, Reported, Signal, Signals};
use fleet_api::task::{LOCAL_HOST, SessionRef};

/// The most of one file Fleet reads, from its end, so a runaway hook cannot
/// stall a pass.
const MAX_FILE: u64 = 8 * 1024 * 1024;

/// Where a session on a host other than this machine reports, under the
/// host's home: `fleet signal` there expands the `~/`, which this machine
/// cannot, as it does not know that home.
pub const PLACE_DIR: &str = "~/.local/state/fleet/signals";

/// The file a session under `key` on another host reports to, as that
/// host's `fleet signal` reads it; `None` for a key that is not a plain
/// name.
pub fn place_file(key: &str) -> Option<String> {
    let plain = !key.is_empty() && !key.starts_with('.') && !key.contains(['/', '\\']);
    plain.then(|| format!("{PLACE_DIR}/{key}.jsonl"))
}

/// The line that parts what a place's signal file held at a restart from
/// what came after, in one read of it ([`read_split`]): not JSON, so no
/// report is ever it.
pub const RESTART_SPLIT: &str = "@@fleet-restart@@";

/// The most of a place's signal file one read returns: the end of what it
/// held at a restart, and the end of what came after. Together well under
/// the 1 MiB an `exec` answer keeps of a stream, so neither the newest
/// report nor the line between the two is cut.
pub const PLACE_READ_BEFORE: u64 = 256 * 1024;
pub const PLACE_READ_AFTER: u64 = 512 * 1024;

/// What `fleet signal-read` prints of the signal file `file` on its own
/// machine (`~`-rooted on a place): the end of what it held before the
/// offset `since`, [`RESTART_SPLIT`] on a line of its own, then the end of
/// what came after. Each part is a byte range computed from the file's
/// length and read by seeking, so no read scans the whole file. A file
/// shorter than `since` was made again: all of it came after. `None` when
/// the file is not there.
pub fn read_split(file: &Path, since: Option<u64>) -> io::Result<Option<Vec<u8>>> {
    let mut f = match fs::File::open(expand_home(file)) {
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

/// The byte length of the signal file `file` (`~`-rooted on a place), 0
/// when it is not there: where a restart starts reading new reports from.
pub fn file_length(file: &Path) -> io::Result<u64> {
    match fs::metadata(expand_home(file)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        meta => Ok(meta?.len()),
    }
}

/// `file` as `fleet signal` opens it: a leading `~/` is the user's home.
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
    /// The adapter over `dir`, made when missing, its owner's only.
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

/// Where a restart left a place's signal file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// Its length then: what it holds past it is the new process's.
    At(u64),
    /// Not known, as on a place whose file Fleet cannot read: nothing the
    /// file holds is taken for the new process's.
    Unknown,
}

/// How a mark of an unknown boundary is written.
const UNKNOWN_BOUNDARY: &str = "unknown";

impl FileSignals {
    /// Keep `boundary`, where the place's signal file was when the session
    /// under `key` on another host was last started: what the file holds
    /// past it is the new process's. Fleet does not write that file, so the
    /// restart is kept here, and as a length rather than a time, so a
    /// place's clock need not agree with Fleet's.
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

    /// Where the place's file was at the session's last start, as
    /// [`FileSignals::mark_started`] kept it; `None` when it was never
    /// restarted. A mark that cannot be read is an unknown boundary.
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

/// Append `signal` to `file` as one line, in one write, so lines from hooks
/// running at once never interleave.
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
            let mut file = fs::File::open(&path)?;
            // Only the tail of a long file: the last state is at its end,
            // and the cut line is skipped as unreadable.
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

    /// A read of a 64 MiB sparse file with the restart offset in its middle
    /// returns its two bounded ranges and the split between them, well
    /// under the 1 MiB `exec` keeps, and the new process's report is the
    /// one folded. Its length is the file's; a missing file reads `None`
    /// and has length 0.
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
        // An offset past the end: the file was made again, all of it new.
        let out = read_split(&path, Some(size + 1))
            .expect("read")
            .expect("there");
        assert!(String::from_utf8_lossy(&out).starts_with(&format!("\n{RESTART_SPLIT}\n")));
        let gone = dir.join("none.jsonl");
        assert_eq!(read_split(&gone, None).expect("read"), None);
        assert_eq!(file_length(&gone).expect("length"), 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
