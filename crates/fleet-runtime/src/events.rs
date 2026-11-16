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
/// most [`MAX_OUTBOX_READ`] bytes; from its start when it is shorter than
/// `from`. `None` when it is not there.
pub fn read_from(file: &Path, from: u64) -> io::Result<Option<OutboxRead>> {
    let mut f = match fs::File::open(expand_home(file)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        opened => opened?,
    };
    let start = if from <= f.metadata()?.len() { from } else { 0 };
    f.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    f.take(MAX_OUTBOX_READ).read_to_end(&mut bytes)?;
    Ok(Some(OutboxRead { start, bytes }))
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
    use crate::FileSignals;
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
        FileSignals::open(&signals).expect("open");
        assert_eq!(mode(&signals), 0o700);
        let _ = fs::remove_dir_all(&root);
    }
}
