use std::io::{BufRead, BufReader, Write};

use fleet_api::board::{self, BoardSpec, BoardStatus};
use fleet_api::event::{self, Appended, Event, Page, Publish, Source};

use super::*;

const SEGMENT_SUFFIX: &str = ".jsonl";
const KEYS_SUFFIX: &str = ".keys";
const SEQ_DIGITS: usize = 20;
const READ_TRIES: u32 = 3;

struct Segment {
    first: u64,
    path: PathBuf,
}

impl Segment {
    fn at(dir: &Path, first: u64) -> Segment {
        Segment {
            first,
            path: dir.join(format!("{first:0SEQ_DIGITS$}{SEGMENT_SUFFIX}")),
        }
    }

    fn keys_path(&self) -> PathBuf {
        self.path.with_extension(&KEYS_SUFFIX[1..])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyEntry {
    source: Source,
    key: String,
    seq: u64,
}

impl KeyEntry {
    fn of(e: &Event) -> Option<KeyEntry> {
        e.key.as_ref().map(|key| KeyEntry {
            source: e.source.clone(),
            key: key.clone(),
            seq: e.seq,
        })
    }
}

struct NewestSegment {
    segment: Segment,
    events: Vec<Event>,
}

impl FileStore {
    fn board_dir(&self, name: &str) -> PathBuf {
        self.root.join("boards").join(name)
    }

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
            if let Some(first) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(segment_first_seq)
            {
                segments.push(Segment { first, path });
            }
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
    ) -> Result<Vec<Appended>, StoreError> {
        if !kinds::may_append_events(&by, source) {
            return Err(StoreError::Forbidden(format!(
                "{} does not publish events as {}",
                by.name(),
                source.name()
            )));
        }
        let _guard = self.lock()?;
        let mut obj = self.read_existing(board::KIND, name)?;
        if obj.is_deleting() {
            return Err(StoreError::Forbidden(format!(
                "{} is being deleted",
                obj.key()
            )));
        }
        let spec: BoardSpec = fleet_api::validate::typed(board::KIND, &obj.spec)?;
        for p in &events {
            event::validate_publish(p, spec.max_data_bytes)
                .map_err(|why| StoreError::Invalid(format!("{}: {why}", obj.key())))?;
        }
        let status = board_status(&obj)?;
        let mut sealed = self.segments(name)?;
        let mut newest = match sealed.pop() {
            Some(segment) => Some(repair_newest(&self.tmp_dir(), segment)?),
            None => None,
        };
        let mut head = status
            .head
            .max(newest.as_ref().map_or(0, NewestSegment::last_seq));
        let now = self.now();
        let mut dropped = drop_aged(&spec, &mut sealed, &mut newest, now)?;

        let held = if events.iter().any(|p| p.key.is_some()) {
            held_keys(&self.tmp_dir(), &sealed, newest.as_ref())?
        } else {
            Vec::new()
        };
        let mut fresh: Vec<Event> = Vec::new();
        let mut answers = Vec::new();
        for p in events {
            let held_seq = p.key.as_deref().and_then(|key| {
                held.iter()
                    .find(|k| k.key == key && k.source == *source)
                    .map(|k| k.seq)
                    .or_else(|| fresh.iter().find(|e| e.same_key(source, &p)).map(|e| e.seq))
            });
            let answer = match held_seq {
                Some(seq) => Appended {
                    event: self.event_at(&sealed, newest.as_ref(), &fresh, seq)?,
                    duplicate: true,
                },
                None => {
                    head = next_seq(head, &obj)?;
                    let event = Event::new(name, head, now, source.clone(), p);
                    fresh.push(event.clone());
                    Appended {
                        event,
                        duplicate: false,
                    }
                }
            };
            answers.push(answer);
        }

        let mut segments = sealed;
        let open = newest.map(|n| n.segment);
        let open_count = open.as_ref().map_or(0, |s| {
            (head_before(&fresh, head) + 1).saturating_sub(s.first)
        });
        segments.extend(open);
        self.write_events(name, &spec, &mut segments, open_count, &fresh)?;
        dropped.extend(drop_counted(&spec, &mut segments, head));

        if !fresh.is_empty() || !dropped.is_empty() {
            let status = BoardStatus {
                head,
                oldest: segments.first().map_or(head + 1, |s| s.first),
            };
            obj.status =
                serde_json::to_value(status).map_err(|e| StoreError::Invalid(e.to_string()))?;
            obj.metadata.resource_version = self.next_rv()?;
            self.write(&obj)?;
        }
        for s in dropped {
            for path in [s.keys_path(), s.path] {
                durable::remove(&path).map_err(|e| io_unknown(&path, e))?;
            }
        }
        Ok(answers)
    }

    fn event_at(
        &self,
        sealed: &[Segment],
        newest: Option<&NewestSegment>,
        fresh: &[Event],
        seq: u64,
    ) -> Result<Event, StoreError> {
        let in_memory = fresh
            .iter()
            .chain(newest.into_iter().flat_map(|n| n.events.iter()))
            .find(|e| e.seq == seq);
        if let Some(e) = in_memory {
            return Ok(e.clone());
        }
        let segment = sealed
            .iter()
            .rev()
            .find(|s| s.first <= seq)
            .ok_or_else(|| StoreError::Unknown(format!("seq {seq}: no segment holds it")))?;
        read_segment(&segment.path)?
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.seq == seq)
            .ok_or_else(|| {
                StoreError::Unknown(format!(
                    "{}: its key file names seq {seq}, which it does not hold",
                    segment.path.display()
                ))
            })
    }

    fn write_events(
        &self,
        name: &str,
        spec: &BoardSpec,
        segments: &mut Vec<Segment>,
        mut open_count: u64,
        fresh: &[Event],
    ) -> Result<(), StoreError> {
        if fresh.is_empty() {
            return Ok(());
        }
        let dir = self.board_dir(name);
        fs::create_dir_all(&dir).map_err(|e| io_unknown(&dir, e))?;
        let mut pending: Vec<(PathBuf, Vec<u8>, Vec<u8>)> = Vec::new();
        for e in fresh {
            if segments.is_empty() || open_count >= spec.segment_events() {
                segments.push(Segment::at(&dir, e.seq));
                open_count = 0;
            }
            let path = &segments.last().expect("a segment").path;
            if pending.last().is_none_or(|(p, _, _)| p != path) {
                pending.push((path.clone(), Vec::new(), Vec::new()));
            }
            let (_, lines, keys) = pending.last_mut().expect("a pending write");
            push_line(lines, e)?;
            if let Some(k) = KeyEntry::of(e) {
                push_line(keys, &k)?;
            }
            open_count += 1;
        }
        for (path, lines, keys) in pending {
            append(&path, &lines).map_err(|e| io_unknown(&path, e))?;
            let keys_path = path.with_extension(&KEYS_SUFFIX[1..]);
            append(&keys_path, &keys).map_err(|e| io_unknown(&keys_path, e))?;
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

impl NewestSegment {
    fn last_seq(&self) -> u64 {
        self.events.last().map_or(self.segment.first - 1, |e| e.seq)
    }
}

fn segment_first_seq(file_name: &str) -> Option<u64> {
    file_name
        .strip_suffix(SEGMENT_SUFFIX)
        .filter(|digits| digits.len() == SEQ_DIGITS && digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|digits| digits.parse::<u64>().ok())
        .filter(|first| *first > 0)
}

fn next_seq(head: u64, obj: &Object) -> Result<u64, StoreError> {
    head.checked_add(1)
        .ok_or_else(|| StoreError::Invalid(format!("{}: seq exhausted", obj.key())))
}

fn head_before(fresh: &[Event], head: u64) -> u64 {
    fresh.first().map_or(head, |e| e.seq - 1)
}

fn board_status(obj: &Object) -> Result<BoardStatus, StoreError> {
    if obj.status.is_null() {
        return Ok(BoardStatus::default());
    }
    serde_json::from_value(obj.status.clone())
        .map_err(|e| StoreError::Unknown(format!("{}: corrupt status: {e}", obj.key())))
}

fn push_line<T: Serialize>(bytes: &mut Vec<u8>, value: &T) -> Result<(), StoreError> {
    serde_json::to_writer(&mut *bytes, value).map_err(|e| StoreError::Invalid(e.to_string()))?;
    bytes.push(b'\n');
    Ok(())
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

fn complete_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let complete = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(&bytes[..0], |end| &bytes[..=end]);
    complete
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
}

fn read_segment(path: &Path) -> Result<Option<Vec<Event>>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_unknown(path, e)),
    };
    complete_lines(&bytes)
        .map(|line| {
            serde_json::from_slice(line)
                .map_err(|e| StoreError::Unknown(format!("corrupt {}: {e}", path.display())))
        })
        .collect::<Result<Vec<Event>, _>>()
        .map(Some)
}

fn first_event_time(path: &Path) -> Result<Option<Millis>, StoreError> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_unknown(path, e)),
    };
    let mut line = Vec::new();
    BufReader::new(file)
        .read_until(b'\n', &mut line)
        .map_err(|e| io_unknown(path, e))?;
    if line.last() != Some(&b'\n') {
        return Ok(None);
    }
    serde_json::from_slice::<Event>(&line)
        .map(|e| Some(e.time))
        .map_err(|e| StoreError::Unknown(format!("corrupt {}: {e}", path.display())))
}

fn repair_newest(tmp_dir: &Path, segment: Segment) -> Result<NewestSegment, StoreError> {
    let path = &segment.path;
    let bytes = fs::read(path).map_err(|e| io_unknown(path, e))?;
    let keep = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |end| end + 1);
    if keep < bytes.len() {
        let f = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| io_unknown(path, e))?;
        f.set_len(keep as u64)
            .and_then(|()| f.sync_all())
            .map_err(|e| io_unknown(path, e))?;
    }
    let events = complete_lines(&bytes[..keep])
        .map(|line| {
            serde_json::from_slice(line)
                .map_err(|e| StoreError::Unknown(format!("corrupt {}: {e}", path.display())))
        })
        .collect::<Result<Vec<Event>, _>>()?;
    let keys: Vec<KeyEntry> = events.iter().filter_map(KeyEntry::of).collect();
    if read_keys(&segment.keys_path())?.as_ref() != Some(&keys) {
        write_keys(tmp_dir, &segment.keys_path(), &keys)?;
    }
    Ok(NewestSegment { segment, events })
}

fn read_keys(path: &Path) -> Result<Option<Vec<KeyEntry>>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_unknown(path, e)),
    };
    if bytes.last().is_some_and(|b| *b != b'\n') {
        return Ok(None);
    }
    Ok(complete_lines(&bytes)
        .map(serde_json::from_slice)
        .collect::<Result<Vec<KeyEntry>, _>>()
        .ok())
}

fn write_keys(tmp_dir: &Path, path: &Path, keys: &[KeyEntry]) -> Result<(), StoreError> {
    let mut bytes = Vec::new();
    for k in keys {
        push_line(&mut bytes, k)?;
    }
    durable::replace(tmp_dir, path, &bytes).map_err(|e| io_unknown(path, e))
}

fn sealed_keys(tmp_dir: &Path, segment: &Segment) -> Result<Vec<KeyEntry>, StoreError> {
    match read_keys(&segment.keys_path())? {
        Some(keys) => Ok(keys),
        None => {
            let keys: Vec<KeyEntry> = read_segment(&segment.path)?
                .unwrap_or_default()
                .iter()
                .filter_map(KeyEntry::of)
                .collect();
            write_keys(tmp_dir, &segment.keys_path(), &keys)?;
            Ok(keys)
        }
    }
}

fn held_keys(
    tmp_dir: &Path,
    sealed: &[Segment],
    newest: Option<&NewestSegment>,
) -> Result<Vec<KeyEntry>, StoreError> {
    let mut held = Vec::new();
    for s in sealed {
        held.extend(sealed_keys(tmp_dir, s)?);
    }
    held.extend(
        newest
            .into_iter()
            .flat_map(|n| n.events.iter().filter_map(KeyEntry::of)),
    );
    Ok(held)
}

fn drop_aged(
    spec: &BoardSpec,
    sealed: &mut Vec<Segment>,
    newest: &mut Option<NewestSegment>,
    now: Millis,
) -> Result<Vec<Segment>, StoreError> {
    let aged = |time: Millis| time.saturating_add(spec.retention.max_age_ms()) < now;
    let mut dropped = Vec::new();
    while !sealed.is_empty() {
        let next_first_time = match sealed.get(1) {
            Some(next) => first_event_time(&next.path)?,
            None => newest
                .as_ref()
                .and_then(|n| n.events.first().map(|e| e.time)),
        };
        if !next_first_time.is_some_and(aged) {
            break;
        }
        dropped.push(sealed.remove(0));
    }
    if sealed.is_empty()
        && newest
            .as_ref()
            .and_then(|n| n.events.last())
            .is_some_and(|e| aged(e.time))
    {
        dropped.extend(newest.take().map(|n| n.segment));
    }
    Ok(dropped)
}

fn drop_counted(spec: &BoardSpec, segments: &mut Vec<Segment>, head: u64) -> Vec<Segment> {
    let mut dropped = Vec::new();
    while segments.len() > 1 && head - segments[0].first + 1 > spec.retention.max_count {
        dropped.push(segments.remove(0));
    }
    dropped
}
