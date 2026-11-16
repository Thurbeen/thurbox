//! Each Board's log: `boards/<name>/<first seq>.jsonl` segments, one event
//! per line, appended only under the store lock. Seqs are contiguous, so a
//! segment's name and the next one's say how many events it holds.
//!
//! A line reaches disk with a single append and an fsync. A crash can leave
//! a torn last line: a reader skips a line with no newline, and the next
//! append cuts it off before it writes. The log is the truth for `head`; the
//! Board's status mirrors it after each append.

use std::io::Write;

use fleet_api::board::{self, BoardSpec, BoardStatus};
use fleet_api::event::{self, Appended, Event, Page, Publish, Source};

use super::*;

const SEGMENT_SUFFIX: &str = ".jsonl";
const READ_TRIES: u32 = 3;

struct Segment {
    first: u64,
    path: PathBuf,
}

impl FileStore {
    fn board_dir(&self, name: &str) -> PathBuf {
        self.root.join("boards").join(name)
    }

    /// Remove a Board's log, so a Board made again under the name starts
    /// empty. Absent is fine.
    pub(super) fn remove_board_log(&self, name: &str) -> Result<(), StoreError> {
        let dir = self.board_dir(name);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_unknown(&dir, e)),
        }
    }

    fn segments(&self, name: &str) -> Result<Vec<Segment>, StoreError> {
        let dir = self.board_dir(name);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_unknown(&dir, e)),
        };
        let mut segments = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| io_unknown(&dir, e))?.path();
            let first = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(SEGMENT_SUFFIX))
                .and_then(|n| n.parse::<u64>().ok())
                .filter(|first| *first > 0)
                .ok_or_else(|| {
                    StoreError::Unknown(format!(
                        "unexpected file {} in a board log",
                        path.display()
                    ))
                })?;
            segments.push(Segment { first, path });
        }
        segments.sort_by_key(|s| s.first);
        Ok(segments)
    }

    pub(super) fn append_board_events(
        &self,
        name: &str,
        source: &Source,
        events: Vec<Publish>,
        by: FieldManager,
        lease: Option<&LeaseToken>,
    ) -> Result<Vec<Appended>, StoreError> {
        if !kinds::may_append_events(&by, source) {
            return Err(StoreError::Forbidden(format!(
                "{} does not publish events as {}",
                by.name(),
                source.name()
            )));
        }
        let _guard = self.lock()?;
        if by.is_controller() {
            match lease {
                Some(l) => self.check_lease(l)?,
                None => {
                    return Err(StoreError::LeaseLost(format!(
                        "{} writes only under the manager lease",
                        by.name()
                    )));
                }
            }
        }
        let mut obj = self.read_existing(board::KIND, name)?;
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted",
                obj.key()
            )));
        }
        let spec: BoardSpec = fleet_api::validate::typed(board::KIND, &obj.spec)?;
        let reserved = kinds::may_publish_reserved(&by);
        for p in &events {
            event::validate_publish(p, spec.max_data_bytes, reserved)
                .map_err(|why| StoreError::Invalid(format!("{}: {why}", obj.key())))?;
        }
        let mut segments = self.segments(name)?;
        let tail = match segments.last() {
            Some(s) => repair_tail(s)?,
            None => None,
        };
        let mut head = board_status(&obj)?
            .head
            .max(tail.map_or(0, |e| e.seq))
            .max(segments.last().map_or(0, |s| s.first - 1));
        let held = if events.iter().any(|p| p.key.is_some()) {
            read_all(&segments)?
        } else {
            Vec::new()
        };
        let now = self.now();
        let mut fresh: Vec<Event> = Vec::new();
        let mut answers = Vec::new();
        for p in events {
            if let Some(e) = held.iter().chain(&fresh).find(|e| e.same_key(source, &p)) {
                answers.push(Appended {
                    event: e.clone(),
                    duplicate: true,
                });
                continue;
            }
            head = head
                .checked_add(1)
                .ok_or_else(|| StoreError::Invalid(format!("{}: seq exhausted", obj.key())))?;
            let e = Event::new(name, head, now, source.clone(), p);
            fresh.push(e.clone());
            answers.push(Appended {
                event: e,
                duplicate: false,
            });
        }
        if fresh.is_empty() {
            return Ok(answers);
        }
        self.write_events(name, &spec, &mut segments, &fresh)?;
        drop_expired(&spec, &mut segments, head, now)?;
        let status = BoardStatus {
            head,
            oldest: segments.first().map_or(head + 1, |s| s.first),
        };
        obj.status =
            serde_json::to_value(status).map_err(|e| StoreError::Invalid(e.to_string()))?;
        obj.metadata.resource_version = self.next_rv()?;
        self.write(&obj)?;
        Ok(answers)
    }

    fn write_events(
        &self,
        name: &str,
        spec: &BoardSpec,
        segments: &mut Vec<Segment>,
        fresh: &[Event],
    ) -> Result<(), StoreError> {
        let dir = self.board_dir(name);
        fs::create_dir_all(&dir).map_err(|e| io_unknown(&dir, e))?;
        let mut pending: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        for e in fresh {
            let full = segments
                .last()
                .is_none_or(|s| e.seq - s.first >= spec.segment_events());
            if full {
                let path = dir.join(format!("{:020}{SEGMENT_SUFFIX}", e.seq));
                segments.push(Segment {
                    first: e.seq,
                    path: path.clone(),
                });
                pending.push((path, Vec::new()));
            }
            let path = &segments.last().expect("a segment").path;
            if pending.last().is_none_or(|(p, _)| p != path) {
                pending.push((path.clone(), Vec::new()));
            }
            let bytes = &mut pending.last_mut().expect("a pending write").1;
            serde_json::to_writer(&mut *bytes, e)
                .map_err(|e| StoreError::Invalid(e.to_string()))?;
            bytes.push(b'\n');
        }
        for (path, bytes) in pending {
            append(&path, &bytes).map_err(|e| io_unknown(&path, e))?;
        }
        Ok(())
    }

    pub(super) fn read_board_events(
        &self,
        name: &str,
        after: u64,
        limit: usize,
    ) -> Result<Page, StoreError> {
        for _ in 0..READ_TRIES {
            let obj = match self.read(board::KIND, name) {
                Observation::Known(Some(o)) => o,
                Observation::Known(None) => {
                    return Err(StoreError::NotFound(format!("{}/{name}", board::KIND)));
                }
                Observation::Unknown { why } => return Err(StoreError::Unknown(why)),
            };
            let head = board_status(&obj)?.head;
            let segments = self.segments(name)?;
            let oldest = segments.first().map_or(head + 1, |s| s.first);
            let next = after.saturating_add(1);
            if next < oldest {
                return Ok(Page::Expired { oldest });
            }
            let start = segments.iter().rposition(|s| s.first <= next).unwrap_or(0);
            let mut out = Vec::new();
            let mut changed = false;
            for s in &segments[start..] {
                let Some(events) = read_segment(&s.path)? else {
                    changed = true;
                    break;
                };
                out.extend(events.into_iter().filter(|e| e.seq > after));
                if out.len() >= limit {
                    out.truncate(limit);
                    return Ok(Page::Events(out));
                }
            }
            if !changed {
                return Ok(Page::Events(out));
            }
        }
        Err(StoreError::Busy(format!(
            "{}/{name}: the log kept changing under the read",
            board::KIND
        )))
    }
}

fn board_status(obj: &Object) -> Result<BoardStatus, StoreError> {
    if obj.status.is_null() {
        return Ok(BoardStatus::default());
    }
    serde_json::from_value(obj.status.clone())
        .map_err(|e| StoreError::Unknown(format!("{}: corrupt status: {e}", obj.key())))
}

fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let created = !path.exists();
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    if created {
        durable::sync_parent(path)?;
    }
    Ok(())
}

/// The events of a segment's complete lines; `None` when the segment is
/// gone, which only retention does.
fn read_segment(path: &Path) -> Result<Option<Vec<Event>>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_unknown(path, e)),
    };
    let complete = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(&bytes[..0], |end| &bytes[..=end]);
    complete
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_slice(line)
                .map_err(|e| StoreError::Unknown(format!("corrupt {}: {e}", path.display())))
        })
        .collect::<Result<Vec<Event>, _>>()
        .map(Some)
}

fn read_all(segments: &[Segment]) -> Result<Vec<Event>, StoreError> {
    let mut all = Vec::new();
    for s in segments {
        all.extend(read_segment(&s.path)?.unwrap_or_default());
    }
    Ok(all)
}

/// Cut a torn last line off the newest segment, and answer its last event.
fn repair_tail(s: &Segment) -> Result<Option<Event>, StoreError> {
    let bytes = fs::read(&s.path).map_err(|e| io_unknown(&s.path, e))?;
    let keep = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |end| end + 1);
    if keep < bytes.len() {
        let f = fs::OpenOptions::new()
            .write(true)
            .open(&s.path)
            .map_err(|e| io_unknown(&s.path, e))?;
        f.set_len(keep as u64)
            .and_then(|()| f.sync_all())
            .map_err(|e| io_unknown(&s.path, e))?;
    }
    Ok(read_segment(&s.path)?.and_then(|events| events.into_iter().last()))
}

/// Drop the oldest segments while the log holds more than `maxCount`
/// events or the segment's newest event is older than `maxAgeSeconds`. The
/// newest segment, which holds the event just appended, always stays.
fn drop_expired(
    spec: &BoardSpec,
    segments: &mut Vec<Segment>,
    head: u64,
    now: Millis,
) -> Result<(), StoreError> {
    while segments.len() > 1 {
        let oldest = &segments[0];
        let too_many = head - oldest.first + 1 > spec.retention.max_count;
        let too_old = || -> Result<bool, StoreError> {
            let newest = read_segment(&oldest.path)?
                .and_then(|events| events.last().map(|e| e.time))
                .unwrap_or(0);
            Ok(newest.saturating_add(spec.retention.max_age_ms()) < now)
        };
        if !too_many && !too_old()? {
            break;
        }
        durable::remove(&oldest.path).map_err(|e| io_unknown(&oldest.path, e))?;
        segments.remove(0);
    }
    Ok(())
}
