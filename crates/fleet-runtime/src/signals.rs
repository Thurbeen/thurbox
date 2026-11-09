//! The signal port's adapter: one file per session under Fleet's data
//! directory, on this machine.
//!
//! A session on the local host gets `<dir>/<key>.jsonl` and its hooks
//! append to it through `fleet signal`. A session on any other host gets
//! no file: reading one back there takes the runtime's `exec`, which this
//! adapter does not have, so what Fleet would learn from it stays Unknown.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fleet_api::Observation;
use fleet_api::signal::{MAX_LINE, Reported, Signal, Signals};
use fleet_api::task::{LOCAL_HOST, SessionRef};

/// The most of one file Fleet reads, from its end, so a runaway hook cannot
/// stall a pass.
const MAX_FILE: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct FileSignals {
    dir: PathBuf,
}

impl FileSignals {
    /// The adapter over `dir`, made when missing.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<FileSignals> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(FileSignals { dir })
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        let plain = !key.is_empty() && !key.starts_with('.') && !key.contains(['/', '\\']);
        plain.then(|| self.dir.join(format!("{key}.jsonl")))
    }
}

/// Append `signal` to `file` as one line, in one write, so lines from hooks
/// running at once never interleave.
pub fn append(file: &Path, signal: &Signal) -> io::Result<()> {
    let mut line = serde_json::to_vec(signal).map_err(io::Error::other)?;
    line.push(b'\n');
    if line.len() > MAX_LINE {
        return Err(io::Error::other("the signal is too long"));
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?
        .write_all(&line)
}

impl Signals for FileSignals {
    fn file(&self, host: &str, key: &str) -> Option<String> {
        if host != LOCAL_HOST {
            return None;
        }
        self.path(key).map(|p| p.to_string_lossy().into_owned())
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
