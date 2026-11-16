//! A session's outbox: the file `fleet event publish` appends to inside a
//! session, beside the session's signal file, and Fleet's reads of it.
//!
//! ```text
//! local:  fleet event publish ─▶ <data>/signals/<key>.jsonl.events ◀─read── Fleet
//! remote: fleet event publish ─▶ ~/.local/state/fleet/signals/<key>.jsonl.events
//!                                   ◀─runtime exec (fleet signal-read --from)── Fleet
//! ```

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use fleet_api::event::{MAX_OUTBOX_READ, OutboxLine, OutboxRead};

use crate::signals::expand_home;

/// Make `dir` and its missing parents, each readable by its owner only:
/// an outbox holds event payloads, which are private.
pub fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Append `line` to the outbox `file` (`~`-rooted on a place) in one write,
/// so lines from publishes running at once never interleave. A new outbox
/// and a new directory are its owner's only. Answers the offset the line
/// starts at.
pub fn append(file: &Path, line: &OutboxLine) -> io::Result<u64> {
    let bytes = line.encode().map_err(io::Error::other)?;
    let file = expand_home(file);
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir_all(dir)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut f = options.open(&file)?;
    f.write_all(&bytes)?;
    Ok(f.stream_position()? - bytes.len() as u64)
}

/// The outbox `file` (`~`-rooted on a place) from the offset `from`, at
/// most [`MAX_OUTBOX_READ`] bytes, as UTF-8 with no byte added or removed;
/// from its start when it is shorter than `from`. `None` when it is not
/// there; an error when it is not a regular file, a symlink included.
pub fn read_from(file: &Path, from: u64) -> io::Result<Option<OutboxRead>> {
    let mut f = match open_regular(&expand_home(file)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        opened => opened?,
    };
    let start = if from <= f.metadata()?.len() { from } else { 0 };
    f.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    f.take(MAX_OUTBOX_READ).read_to_end(&mut bytes)?;
    Ok(Some(OutboxRead {
        start,
        bytes: with_invalid_utf8_as_nul(bytes),
    }))
}

/// Open `path` for reading only when it is a regular file. A session
/// controls its outbox and signal file: a FIFO with no writer would block
/// the open, and a symlink would point the read at another file this
/// process can read. On Unix the open neither blocks nor follows a symlink.
pub(crate) fn open_regular(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOFOLLOW,
    );
    #[cfg(not(unix))]
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let f = options.open(path)?;
    if !f.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(f)
}

/// `bytes` with each byte that is not part of valid UTF-8 replaced by NUL:
/// a place's read crosses the runtime as text, and this keeps every byte
/// offset as it is in the file. JSON takes no raw NUL, in a string or out
/// of one, so a line it touches no longer parses, and is skipped.
fn with_invalid_utf8_as_nul(mut bytes: Vec<u8>) -> Vec<u8> {
    let mut at = 0;
    while let Err(e) = std::str::from_utf8(&bytes[at..]) {
        let bad = at + e.valid_up_to();
        let end = e.error_len().map_or(bytes.len(), |n| bad + n);
        bytes[bad..end].fill(0);
        at = end;
    }
    bytes
}

/// The key of the session whose outbox is `file`, read back from the name
/// Fleet gave it: [`outbox_file`](fleet_api::event::outbox_file) of the
/// session's `<key>.jsonl` signal file. `None` for any other name.
pub fn outbox_session_key(file: &Path) -> Option<String> {
    let name = file.file_name()?.to_str()?;
    let key = name.strip_suffix(".jsonl.events")?;
    (!key.is_empty()).then(|| key.to_string())
}

/// What `fleet signal-read --from` prints of a read: the offset it starts
/// at on a line of its own, then the bytes.
pub fn encode_place_read(read: &OutboxRead) -> Vec<u8> {
    let mut out = format!("{}\n", read.start).into_bytes();
    out.extend_from_slice(&read.bytes);
    out
}

/// A read back from what `fleet signal-read --from` printed.
pub fn decode_place_read(text: &str) -> Option<OutboxRead> {
    let (start, bytes) = text.split_once('\n')?;
    Some(OutboxRead {
        start: start.parse().ok()?,
        bytes: bytes.as_bytes().to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_api::event::Publish;

    fn line(t: &str) -> OutboxLine {
        OutboxLine::new(
            "shared",
            Publish {
                event_type: t.into(),
                subject: None,
                key: None,
                correlation: None,
                data: serde_json::json!(1),
            },
        )
    }

    #[test]
    fn an_outbox_names_the_session_key_fleet_gave_it_on_each_host() {
        use fleet_api::event::outbox_file;
        use fleet_api::signal::Signals;
        let dir = std::env::temp_dir().join(format!("fleet-outbox-key-{}", std::process::id()));
        let signals = crate::signals::FileSignals::open(&dir).expect("signals");
        let key = "build.v1.0c1e-uid";
        for host in [fleet_api::task::LOCAL_HOST, "box/main"] {
            let outbox = outbox_file(&signals.file(host, key).expect("a signal file"));
            assert_eq!(
                outbox_session_key(Path::new(&outbox)).as_deref(),
                Some(key),
                "{host}"
            );
        }
        for other in ["k.jsonl", "k.events", ".jsonl.events", "dir/"] {
            assert_eq!(outbox_session_key(Path::new(other)), None, "{other}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_append_answers_where_its_line_starts_and_a_read_continues_there() {
        let dir = std::env::temp_dir().join(format!("fleet-outbox-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("k.jsonl.events");
        let _ = fs::remove_file(&file);
        assert_eq!(read_from(&file, 0).expect("read"), None);
        assert_eq!(append(&file, &line("a")).expect("append"), 0);
        let second = append(&file, &line("b")).expect("append");
        assert!(second > 0);
        let all = read_from(&file, 0).expect("read").expect("there");
        assert_eq!(all.start, 0);
        let tail = read_from(&file, second).expect("read").expect("there");
        assert_eq!(tail.start, second);
        assert_eq!(tail.bytes, all.bytes[second as usize..]);
        let past = read_from(&file, all.bytes.len() as u64 + 1)
            .expect("read")
            .expect("there");
        assert_eq!(
            past, all,
            "a file shorter than the offset reads from its start"
        );
        let text = String::from_utf8(encode_place_read(&tail)).expect("utf-8");
        assert_eq!(decode_place_read(&text), Some(tail));
        assert!(append(&file, &line("fleet.x")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn an_append_makes_a_private_outbox_in_a_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("fleet-outbox-mode-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let file = root.join("signals").join("k.jsonl.events");
        append(&file, &line("a")).expect("append");
        let mode = |p: &Path| fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
        let signals = root.join("fleet-signals");
        crate::FileSignals::open(&signals).expect("open");
        assert_eq!(mode(&signals), 0o700);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_line_that_is_not_utf8_keeps_every_offset_across_a_place_read() {
        let dir = std::env::temp_dir().join(format!("fleet-outbox-utf8-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("k.jsonl.events");
        let _ = fs::remove_file(&file);
        append(&file, &line("a")).expect("append");
        OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("open")
            .write_all(b"{\"board\":\"shared\",\"type\":\"\xff\xfe\",\"data\":1}\n")
            .expect("bad line");
        OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("open")
            .write_all(b"{\"board\":\"shared\",\"type\":\"c\",\"data\":\"\xff\"}\n")
            .expect("bad data");
        append(&file, &line("b")).expect("append");
        let len = fs::metadata(&file).expect("meta").len();
        let read = read_from(&file, 0).expect("read").expect("there");
        let sent = encode_place_read(&read);
        let received = decode_place_read(&String::from_utf8_lossy(&sent)).expect("decoded");
        assert_eq!(received.bytes.len() as u64, len);
        let (cursor, entries) =
            fleet_api::event::ingest(&fleet_api::event::OutboxCursor::default(), &received);
        let types: Vec<_> = entries
            .iter()
            .map(|e| e.line.as_ref().map(|(_, p)| p.event_type.clone()).ok())
            .collect();
        assert_eq!(types, vec![Some("a".into()), None, None, Some("b".into())]);
        assert_eq!(entries.last().expect("last").end, len);
        let again = read_from(&file, len).expect("read").expect("there");
        assert_eq!(again.start, len, "no generation bump");
        assert_eq!(cursor.generation, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_outbox_is_refused_at_once() {
        let dir = std::env::temp_dir().join(format!("fleet-outbox-fifo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let fifo = dir.join("k.jsonl.events");
        let made = fleet_exec::run(
            &fleet_exec::Spec::new("mkfifo", std::time::Duration::from_secs(5)).arg(&fifo),
        );
        assert!(
            matches!(made, fleet_exec::Outcome::Exited { code: Some(0), .. }),
            "{made:?}"
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || tx.send(read_from(&path, 0).map(|_| ())).ok());
        let read = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the read returns instead of blocking");
        assert!(read.is_err(), "a FIFO is not an outbox");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_outbox_is_refused() {
        let dir = std::env::temp_dir().join(format!("fleet-outbox-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let secret = dir.join("secret");
        fs::write(
            &secret,
            "{\"board\":\"shared\",\"type\":\"a\",\"data\":1}\n",
        )
        .expect("secret");
        let link = dir.join("k.jsonl.events");
        std::os::unix::fs::symlink(&secret, &link).expect("symlink");
        assert!(read_from(&link, 0).is_err(), "a symlink is not followed");
        let _ = fs::remove_dir_all(&dir);
    }
}
