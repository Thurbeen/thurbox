use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const WANT_CONTENT: &str = "want_content";

pub const WANT_SESSIONS: &str = "want_content.sessions";

pub const HITS_PER_SESSION: usize = 50;

pub const MAX_HITS: usize = 200;

pub const SNIPPET_CHARS: usize = 160;

const SNIPPET_LEAD: usize = 24;

pub const RESCAN_INTERVAL: Duration = Duration::from_secs(1);

// ── Query ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Term {
    Word(String),
    Phrase(String),
    Regex(regex::Regex),
}

#[derive(Debug, Clone)]
pub struct Query {
    terms: Vec<Term>,
    case_sensitive: bool,
    whole: Option<String>,
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

pub fn fold_line(line: &str) -> String {
    if line.is_ascii() {
        return line.to_ascii_lowercase();
    }
    line.chars().map(fold).collect()
}

impl Query {
    pub fn parse(text: &str) -> Result<Option<Self>, String> {
        let case_sensitive = text.chars().any(char::is_uppercase);
        let prepare = |s: &str| -> String {
            if case_sensitive {
                s.to_string()
            } else {
                fold_line(s)
            }
        };
        let mut terms = Vec::new();
        let mut words: Vec<String> = Vec::new();
        let mut rest = text.trim_start();
        while !rest.is_empty() {
            if let Some(quoted) = rest.strip_prefix('"') {
                let (phrase, after) = quoted.split_once('"').unwrap_or((quoted, ""));
                if !phrase.is_empty() {
                    terms.push(Term::Phrase(prepare(phrase)));
                }
                rest = after.trim_start();
                continue;
            }
            if let Some(close) = regex_end(rest) {
                let pattern = &rest[1..close];
                rest = rest[close + 1..].trim_start();
                let regex = regex::RegexBuilder::new(pattern)
                    .case_insensitive(!case_sensitive)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|e| format!("not a valid regex: {e}"))?;
                terms.push(Term::Regex(regex));
                continue;
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let token = &rest[..end];
            rest = rest[end..].trim_start();
            terms.push(Term::Word(prepare(token)));
            words.push(token.to_string());
        }
        if terms.is_empty() {
            return Ok(None);
        }
        let whole =
            (terms.len() > 1 && words.len() == terms.len()).then(|| prepare(&words.join(" ")));
        Ok(Some(Self {
            terms,
            case_sensitive,
            whole,
        }))
    }

    pub fn match_line(&self, line: &str) -> Option<LineMatch> {
        if self.case_sensitive {
            self.match_folded(line, line)
        } else {
            self.match_folded(line, &fold_line(line))
        }
    }

    pub fn match_folded(&self, line: &str, folded: &str) -> Option<LineMatch> {
        let haystack = if self.case_sensitive { line } else { folded };
        let mut score = 0i32;
        let mut exact = true;
        let mut ranges = Vec::new();
        let mut chars: Option<Vec<char>> = None;
        for term in &self.terms {
            match term {
                Term::Word(word) => {
                    if let Some((at, end, bonus)) = find_str(haystack, word) {
                        score += 100 + bonus;
                        ranges.push((at, end));
                    } else {
                        if !in_order(haystack, word) {
                            return None;
                        }
                        let all = chars.get_or_insert_with(|| haystack.chars().collect());
                        let wanted: Vec<char> = word.chars().collect();
                        let positions = tight_subsequence(all, &wanted)?;
                        let span = positions.last()? + 1 - positions[0];
                        let gaps = i32::try_from(span - wanted.len()).unwrap_or(i32::MAX / 8);
                        score += (40 - gaps.saturating_mul(5)).max(10);
                        exact = false;
                        ranges.extend(positions.iter().map(|&p| (p, p + 1)));
                    }
                }
                Term::Phrase(phrase) => {
                    let (at, end, bonus) = find_str(haystack, phrase)?;
                    score += 150 + bonus;
                    ranges.push((at, end));
                }
                Term::Regex(regex) => {
                    let found = regex.find(line)?;
                    let start = line[..found.start()].chars().count();
                    let len = found.as_str().chars().count();
                    score += 100;
                    ranges.push((start, start + len.max(1)));
                }
            }
        }
        if let Some(whole) = &self.whole {
            if let Some((at, end, _)) = find_str(haystack, whole) {
                score += 200;
                ranges.push((at, end));
            }
        }
        Some(LineMatch {
            score,
            exact,
            ranges: merge(ranges),
        })
    }
}

fn regex_end(text: &str) -> Option<usize> {
    let body = text.strip_prefix('/')?;
    let mut escaped = false;
    for (at, c) in body.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '/' if !escaped => {
                let after = &body[at + 1..];
                if at > 0 && after.chars().next().is_none_or(char::is_whitespace) {
                    return Some(at + 1);
                }
            }
            _ => escaped = false,
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    pub score: i32,
    pub exact: bool,
    pub ranges: Vec<(usize, usize)>,
}

fn find_str(haystack: &str, needle: &str) -> Option<(usize, usize, i32)> {
    if needle.is_empty() {
        return None;
    }
    let byte = haystack.find(needle)?;
    let before = &haystack[..byte];
    let after = &haystack[byte + needle.len()..];
    let open = before
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric());
    let close = after.chars().next().is_none_or(|c| !c.is_alphanumeric());
    let bonus = match (open, close) {
        (true, true) => 30,
        (true, false) | (false, true) => 15,
        _ => 0,
    };
    let at = before.chars().count();
    Some((at, at + needle.chars().count(), bonus))
}

fn in_order(haystack: &str, needle: &str) -> bool {
    // `str::find` with a `char` is a memchr, so this skips from one wanted
    // character to the next instead of stepping through every one between.
    let mut rest = haystack;
    for c in needle.chars() {
        match rest.find(c) {
            Some(at) => rest = &rest[at + c.len_utf8()..],
            None => return false,
        }
    }
    true
}

fn tight_subsequence(haystack: &[char], needle: &[char]) -> Option<Vec<usize>> {
    let (&first, rest) = needle.split_first()?;
    let limit = needle.len() * 2;
    let mut best: Option<(usize, Vec<usize>)> = None;
    for start in (0..haystack.len()).filter(|&i| haystack[i] == first) {
        let end = (start + limit).min(haystack.len());
        let mut positions = vec![start];
        let mut at = start + 1;
        for &wanted in rest {
            match haystack[at.min(end)..end].iter().position(|&c| c == wanted) {
                Some(offset) => {
                    positions.push(at + offset);
                    at += offset + 1;
                }
                None => break,
            }
        }
        let span = at - start;
        if positions.len() == needle.len() && best.as_ref().is_none_or(|(b, _)| span < *b) {
            best = Some((span, positions));
        }
    }
    best.map(|(_, positions)| positions)
}

fn merge(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

// ── History ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLine {
    pub text: String,
    pub folded: String,
    pub row: usize,
    pub wraps: Vec<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub lines: Vec<HistoryLine>,
    pub scrollback: usize,
    pub rows: usize,
    pub size: (u16, u16),
}

fn screen_size(screen: &vt100::Screen) -> (u16, u16) {
    let (rows, cols) = screen.size();
    (rows.get(), cols.get())
}

impl History {
    pub fn read(screen: &mut vt100::Screen) -> Self {
        let mut reading = Reading::fresh(screen_size(screen));
        while !reading.step(screen, usize::MAX) {}
        reading.finish()
    }

    pub fn read_locked(
        parser: &Mutex<crate::backend::SessionParser>,
        cached: Option<&History>,
        stats: &mut ReadStats,
    ) -> Option<Self> {
        let mut reading: Option<Reading> = None;
        loop {
            let mut guard = parser.lock().ok()?;
            let screen = guard.screen_mut();
            let held = Instant::now();
            let reading = reading.get_or_insert_with(|| match cached {
                Some(cached) if cached.size == screen_size(screen) => Reading::resume(cached),
                _ => Reading::fresh(screen_size(screen)),
            });
            let done = reading.step(screen, CHUNK_ROWS);
            drop(guard);
            stats.held = stats.held.max(held.elapsed());
            if done {
                break;
            }
        }
        let reading = reading?;
        stats.holds += reading.holds;
        stats.rows += reading.rows_read;
        stats.widest = stats.widest.max(reading.widest);
        Some(reading.finish())
    }

    pub fn back(&self, row: usize) -> usize {
        (self.scrollback + self.rows).saturating_sub(row + 1)
    }

    pub fn row_on_screen(&self, row: usize) -> usize {
        (row + self.scroll_to(row)).saturating_sub(self.scrollback)
    }

    pub fn scroll_to(&self, row: usize) -> usize {
        if row >= self.scrollback {
            return 0;
        }
        (self.scrollback + self.rows / 3)
            .saturating_sub(row)
            .min(self.scrollback)
    }
}

pub const CHUNK_ROWS: usize = 128;

const MAX_SHIFT: usize = CHUNK_ROWS;

const ANCHOR_ROWS: usize = 4;

const ANCHOR_SPAN: usize = 64;

const MAX_RESTARTS: usize = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadStats {
    pub held: Duration,
    pub holds: usize,
    pub rows: usize,
    pub widest: usize,
}

struct Reading {
    lines: Vec<HistoryLine>,
    next: usize,
    continues: bool,
    anchor: Option<(usize, Vec<String>)>,
    size: (u16, u16),
    scrollback: usize,
    restarts: usize,
    holds: usize,
    rows_read: usize,
    widest: usize,
}

impl Reading {
    fn fresh(size: (u16, u16)) -> Self {
        Self {
            lines: Vec::new(),
            next: 0,
            continues: false,
            anchor: None,
            size,
            scrollback: 0,
            restarts: 0,
            holds: 0,
            rows_read: 0,
            widest: 0,
        }
    }

    fn resume(cached: &History) -> Self {
        let mut reading = Self::fresh(cached.size);
        let keep = cached
            .lines
            .windows(2)
            .take_while(|pair| pair[1].row <= cached.scrollback)
            .count();
        if keep == 0 {
            return reading;
        }
        reading.lines = cached.lines[..keep].to_vec();
        reading.next = cached.lines[keep].row;
        // The rows just above `next`, newest first, out of the lines kept.
        let mut above = reading
            .lines
            .iter()
            .rev()
            .flat_map(|line| line.row_texts().into_iter().rev());
        reading.anchor = Some(anchor_before(reading.next, |_| {
            above.next().unwrap_or_default()
        }));
        reading
    }

    fn restart(&mut self, size: (u16, u16)) {
        let (holds, rows_read, widest) = (self.holds, self.rows_read, self.widest);
        let restarts = self.restarts + 1;
        *self = Self::fresh(size);
        (self.holds, self.rows_read, self.widest, self.restarts) =
            (holds, rows_read, widest, restarts);
    }

    fn step(&mut self, screen: &mut vt100::Screen, budget: usize) -> bool {
        self.holds += 1;
        let offset = screen.scrollback();
        if screen_size(screen) != self.size {
            self.restart(screen_size(screen));
        }
        screen.set_scrollback(usize::MAX);
        let scrollback = screen.scrollback();
        if !self.relocate(screen, scrollback) {
            if self.restarts < MAX_RESTARTS {
                self.restart(self.size);
            }
            self.anchor = None;
        }
        let (rows, cols) = (usize::from(self.size.0), self.size.1);
        let total = scrollback + rows;
        // The screen is read whole, in the same hold as the last of the
        // scrollback: scrollback rows only move, so a chunk of them can be
        // found again, but the screen can be redrawn in place. And a read that
        // stopped at the edge of the scrollback would, against an agent that
        // prints between every hold, spend each one catching up on the rows
        // just printed and never reach the screen at all.
        let end = if self.next.saturating_add(budget) >= scrollback {
            total
        } else {
            self.next + budget
        };
        let from = self.next;
        let mut start = from;
        while start < end && rows > 0 {
            // At offset `k` the viewport's first row is history row
            // `scrollback - k`; in the scrollback that is `start` itself, so
            // no row is built only to be skipped.
            let k = scrollback.saturating_sub(start);
            screen.set_scrollback(k);
            let base = scrollback - k;
            let first = start - base;
            let last = rows.min(end - base);
            for (r, text) in screen.rows(0, cols).enumerate().take(last).skip(first) {
                let row = base + r;
                match self.lines.last_mut() {
                    Some(line) if self.continues => {
                        line.wraps.push(line.text.chars().count());
                        line.text.push_str(&text);
                    }
                    _ => self.lines.push(HistoryLine {
                        text,
                        folded: String::new(),
                        row,
                        wraps: Vec::new(),
                    }),
                }
                self.continues = u16::try_from(r).is_ok_and(|r| screen.row_wrapped(r));
            }
            start = base + last;
        }
        self.next = start;
        self.scrollback = scrollback;
        self.rows_read += start - from;
        self.widest = self.widest.max(start - from);
        self.anchor = (start < total && start > 0).then(|| {
            anchor_before(start, |row| {
                row_text(screen, scrollback, row).unwrap_or_default()
            })
        });
        screen.set_scrollback(offset);
        start >= total
    }

    fn relocate(&mut self, screen: &mut vt100::Screen, scrollback: usize) -> bool {
        let Some((first, texts)) = self.anchor.take() else {
            return true;
        };
        // Rows with text first: a wrong shift is then turned away by the
        // first comparison instead of after a run of matching blank rows.
        let mut order: Vec<usize> = (0..texts.len()).collect();
        order.sort_by_key(|&i| texts[i].trim().is_empty());
        let mut found = (0..=MAX_SHIFT.min(first)).filter(|&shift| {
            order.iter().all(|&i| {
                row_text(screen, scrollback, first - shift + i).as_deref()
                    == Some(texts[i].as_str())
            })
        });
        // The rows must be found at exactly one place. Repeated output can
        // match at two, and picking the nearest would carry on from the wrong
        // row as surely as not looking at all.
        let (Some(shift), None) = (found.next(), found.next()) else {
            return false;
        };
        if shift > 0 {
            // Rows pushed out of the top are gone. A line that lost its first
            // rows keeps the rest, starting where they now start — which is
            // what a read from scratch would make of them.
            self.lines.retain_mut(|line| line.drop_rows(shift));
            for line in &mut self.lines {
                line.row -= shift;
            }
            self.next -= shift;
        }
        true
    }

    fn finish(mut self) -> History {
        for line in &mut self.lines {
            if line.folded.is_empty() && !line.text.is_empty() {
                line.folded = fold_line(&line.text);
            }
        }
        History {
            lines: self.lines,
            scrollback: self.scrollback,
            rows: usize::from(self.size.0),
            size: self.size,
        }
    }
}

fn row_text(screen: &mut vt100::Screen, scrollback: usize, row: usize) -> Option<String> {
    let k = scrollback.saturating_sub(row);
    screen.set_scrollback(k);
    let base = scrollback - k;
    screen
        .rows(0, screen_size(screen).1)
        .nth(row.checked_sub(base)?)
}

fn anchor_before(end: usize, mut text_of: impl FnMut(usize) -> String) -> (usize, Vec<String>) {
    let mut texts = Vec::new();
    let mut solid = 0;
    let mut row = end;
    while row > 0 && solid < ANCHOR_ROWS && texts.len() < ANCHOR_SPAN {
        row -= 1;
        let text = text_of(row);
        solid += usize::from(!text.trim().is_empty());
        texts.push(text);
    }
    texts.reverse();
    (row, texts)
}

impl HistoryLine {
    fn drop_rows(&mut self, row: usize) -> bool {
        if self.row >= row {
            return true;
        }
        let lost = row - self.row;
        if lost > self.wraps.len() {
            return false;
        }
        let at = self.wraps[lost - 1];
        self.text = self.text.chars().skip(at).collect();
        self.folded = String::new();
        self.wraps = self.wraps[lost..].iter().map(|w| w - at).collect();
        self.row = row;
        true
    }

    fn row_texts(&self) -> Vec<String> {
        let chars: Vec<char> = self.text.chars().collect();
        let mut bounds = vec![0];
        bounds.extend(&self.wraps);
        bounds.push(chars.len());
        bounds
            .windows(2)
            .map(|pair| chars[pair[0]..pair[1]].iter().collect())
            .collect()
    }
}

// ── Hits ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub session: String,
    pub shell: bool,
    pub text: String,
    pub ranges: Vec<(usize, usize)>,
    pub back: usize,
    pub scroll: usize,
    pub row: usize,
    pub exact: bool,
    pub score: i32,
}

fn snippet(text: &str, ranges: &[(usize, usize)]) -> (String, Vec<(usize, usize)>) {
    let chars: Vec<char> = text.chars().collect();
    let lead = chars.iter().take_while(|c| c.is_whitespace()).count();
    let tail = chars.iter().rev().take_while(|c| c.is_whitespace()).count();
    let end = chars.len().saturating_sub(tail).max(lead);
    let first = ranges.first().map_or(lead, |r| r.0);
    let mut from = lead;
    let mut prefix = false;
    if end - lead > SNIPPET_CHARS && first > lead + SNIPPET_LEAD {
        from = (first - SNIPPET_LEAD).min(end.saturating_sub(SNIPPET_CHARS));
        prefix = true;
    }
    let to = end.min(from + SNIPPET_CHARS);
    let mut out = String::new();
    if prefix {
        out.push('…');
    }
    out.extend(&chars[from..to]);
    let shift = usize::from(prefix);
    let moved = ranges
        .iter()
        .filter(|(s, e)| *e > from && *s < to)
        .map(|(s, e)| ((*s).max(from) - from + shift, (*e).min(to) - from + shift))
        .collect();
    if to < end {
        out.push('…');
    }
    (out, moved)
}

struct Found {
    source: usize,
    line: usize,
    row: usize,
    matched: LineMatch,
    back: usize,
}

impl Found {
    fn hit(self, sources: &[Source], histories: &[Arc<History>]) -> Hit {
        let history = &histories[self.source];
        let line = &history.lines[self.line];
        let (text, ranges) = snippet(&line.text, &self.matched.ranges);
        Hit {
            session: sources[self.source].session.clone(),
            shell: sources[self.source].shell,
            text,
            ranges,
            back: self.back,
            scroll: history.scroll_to(self.row),
            row: history.row_on_screen(self.row),
            exact: self.matched.exact,
            score: self.matched.score,
        }
    }
}

impl HistoryLine {
    fn row_of(&self, at: usize) -> usize {
        self.row + self.wraps.iter().take_while(|&&start| start <= at).count()
    }
}

fn found_in(query: &Query, source: usize, history: &History) -> Vec<Found> {
    history
        .lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let matched = query.match_folded(&line.text, &line.folded)?;
            // The row the first hit is on, not the row the line starts on: a
            // wrapped line's match can be rows further down.
            let row = line.row_of(matched.ranges.first().map_or(0, |r| r.0));
            Some(Found {
                source,
                line: index,
                row,
                matched,
                back: history.back(row),
            })
        })
        .collect()
}

#[cfg(test)]
fn hits_in(query: &Query, session: &str, shell: bool, history: &History) -> Vec<Hit> {
    let histories = vec![Arc::new(history.clone())];
    let sources = vec![Source {
        session: session.into(),
        shell,
        parser: Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            1.try_into().unwrap(),
            1.try_into().unwrap(),
            0,
            crate::backend::TermSignals::default(),
        ))),
        stamp: 0,
        restore: None,
    }];
    found_in(query, 0, history)
        .into_iter()
        .map(|found| found.hit(&sources, &histories))
        .collect()
}

// ── The worker ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Source {
    pub session: String,
    pub shell: bool,
    pub parser: Arc<Mutex<crate::backend::SessionParser>>,
    pub stamp: u64,
    pub restore: Option<Restore>,
}

pub type Restore = Arc<dyn Fn() -> Option<crate::backend::SessionParser> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Request {
    pub query: String,
    pub sessions: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    pub request: Request,
    pub hits: Vec<Hit>,
    pub total: usize,
    pub sessions: usize,
    pub lines: usize,
    pub elapsed: Duration,
    pub error: Option<String>,
    pub read: ReadStats,
    pub cancelled: bool,
    pub serial: u64,
}

impl Answer {
    fn same(&self, other: &Self) -> bool {
        self.request == other.request
            && self.hits == other.hits
            && self.total == other.total
            && self.sessions == other.sessions
            && self.lines == other.lines
            && self.error == other.error
    }
}

type Cache = Arc<Mutex<CacheMap>>;

const MAX_THREADS: usize = 4;

pub fn run(request: Request, sources: &[Source], cache: &Mutex<CacheMap>) -> Answer {
    run_until(request, sources, cache, &|| false)
}

pub fn run_until(
    request: Request,
    sources: &[Source],
    cache: &Mutex<CacheMap>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Answer {
    let started = Instant::now();
    // A query with no terms still reads: it is how an open strip warms the
    // cache before anything is typed, so the first keystroke is matched
    // against histories already read.
    let query = match Query::parse(&request.query) {
        Ok(query) => query,
        Err(error) => {
            return Answer {
                request,
                error: Some(error),
                ..Answer::default()
            }
        }
    };

    // Most recent output first, per session: the agent and its shell count as
    // one session, and the later of the two says when it was last busy.
    let mut recency: HashMap<&str, u64> = HashMap::new();
    for source in sources {
        let at = recency.entry(source.session.as_str()).or_default();
        *at = (*at).max(source.stamp);
    }

    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get() / 2)
        .clamp(1, MAX_THREADS)
        .min(sources.len().max(1));
    // Interleaved rather than in runs, so the sessions that printed most (and
    // so hold the most to read) do not all land on one thread.
    let searched: Vec<Option<Searched>> = if threads <= 1 {
        (0..sources.len())
            .map(|index| search_one(query.as_ref(), index, sources, cache, cancelled))
            .collect()
    } else {
        let mut slots: Vec<Option<Searched>> = (0..sources.len()).map(|_| None).collect();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    let query = query.as_ref();
                    scope.spawn(move || {
                        (t..sources.len())
                            .step_by(threads)
                            .map(|index| {
                                (index, search_one(query, index, sources, cache, cancelled))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            for handle in handles {
                if let Ok(done) = handle.join() {
                    for (index, searched) in done {
                        slots[index] = searched;
                    }
                }
            }
        });
        slots
    };
    if cancelled() {
        return Answer {
            request,
            cancelled: true,
            ..Answer::default()
        };
    }

    let mut read = ReadStats::default();
    let mut histories: Vec<Arc<History>> = Vec::with_capacity(sources.len());
    let mut per_session: HashMap<&str, Vec<Found>> = HashMap::new();
    let mut lines = 0;
    let mut total = 0;
    for (index, searched) in searched.into_iter().enumerate() {
        let Searched {
            history,
            found,
            stats,
        } = searched.unwrap_or_default();
        read.held = read.held.max(stats.held);
        read.holds += stats.holds;
        read.rows += stats.rows;
        read.widest = read.widest.max(stats.widest);
        lines += history.lines.len();
        total += found.len();
        per_session
            .entry(sources[index].session.as_str())
            .or_default()
            .extend(found);
        histories.push(history);
    }

    let searched = per_session.len();
    let mut ranked: Vec<Found> = Vec::new();
    for (_, mut found) in per_session {
        best(&mut found, HITS_PER_SESSION, |a, b| {
            rank(&a.matched, a.back, &b.matched, b.back)
        });
        ranked.extend(found);
    }
    let session_of = |found: &Found| sources[found.source].session.as_str();
    best(&mut ranked, MAX_HITS, |a, b| {
        rank(&a.matched, 0, &b.matched, 0)
            .then_with(|| recency.get(session_of(b)).cmp(&recency.get(session_of(a))))
            .then(a.back.cmp(&b.back))
            .then(session_of(a).cmp(session_of(b)))
    });
    let hits = ranked
        .into_iter()
        .map(|found| found.hit(sources, &histories))
        .collect();
    Answer {
        request,
        hits,
        total,
        sessions: searched,
        lines,
        elapsed: started.elapsed(),
        read,
        ..Answer::default()
    }
}

fn best<T>(items: &mut Vec<T>, keep: usize, order: impl Fn(&T, &T) -> std::cmp::Ordering) {
    if items.len() > keep {
        items.select_nth_unstable_by(keep, &order);
        items.truncate(keep);
    }
    items.sort_by(order);
}

#[derive(Default)]
struct Searched {
    history: Arc<History>,
    found: Vec<Found>,
    stats: ReadStats,
}

fn search_one(
    query: Option<&Query>,
    index: usize,
    sources: &[Source],
    cache: &Mutex<CacheMap>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Searched> {
    if cancelled() {
        return None;
    }
    let source = &sources[index];
    let key = (source.session.clone(), source.shell);
    let cached = cache.lock().ok().and_then(|c| c.get(&key).cloned());
    let mut stats = ReadStats::default();
    // A read that failed keeps whatever the cache held, and is not stored:
    // stored under this stamp, it would be taken for a good read until the
    // pane printed again.
    let history = match history_of(source, cached.clone(), &mut stats) {
        Some(history) => {
            if let Ok(mut cache) = cache.lock() {
                cache.insert(key, (source.stamp, Arc::clone(&history)));
            }
            history
        }
        None => cached.map(|(_, history)| history).unwrap_or_default(),
    };
    let found = query.map_or_else(Vec::new, |query| found_in(query, index, &history));
    Some(Searched {
        history,
        found,
        stats,
    })
}

fn rank(a: &LineMatch, a_back: usize, b: &LineMatch, b_back: usize) -> std::cmp::Ordering {
    b.exact
        .cmp(&a.exact)
        .then(b.score.cmp(&a.score))
        .then(a_back.cmp(&b_back))
}

pub type CacheMap = HashMap<(String, bool), (u64, Arc<History>)>;

fn history_of(
    source: &Source,
    cached: Option<(u64, Arc<History>)>,
    stats: &mut ReadStats,
) -> Option<Arc<History>> {
    if let Some((stamp, history)) = &cached {
        // A pane with no grid has no size here to compare, and none that
        // changes while it is off screen: its stamp alone says whether it
        // printed since.
        let unchanged = *stamp == source.stamp
            && (source.restore.is_some()
                || source.parser.lock().ok().map(|p| screen_size(p.screen()))
                    == Some(history.size));
        if unchanged {
            return Some(Arc::clone(history));
        }
    }
    let previous = cached.as_ref().map(|(_, history)| history.as_ref());
    let read = match &source.restore {
        Some(restore) => {
            restore().and_then(|parser| History::read_locked(&Mutex::new(parser), previous, stats))
        }
        None => History::read_locked(&source.parser, previous, stats),
    };
    read.map(Arc::new)
}

pub struct SearchStore {
    tx: Sender<Answer>,
    rx: Receiver<Answer>,
    cache: Cache,
    running: Option<Request>,
    ticket: Arc<AtomicU64>,
    answer: Option<Answer>,
    dispatched: Option<Instant>,
    generation: u64,
    serial: u64,
    holding: bool,
}

impl Default for SearchStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchStore {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            cache: Arc::default(),
            running: None,
            ticket: Arc::default(),
            answer: None,
            dispatched: None,
            generation: 0,
            serial: 0,
            holding: false,
        }
    }

    pub fn answer(&self) -> Option<&Answer> {
        self.answer.as_ref()
    }

    pub fn serve(
        &mut self,
        request: Option<Request>,
        generation: u64,
        sources: impl FnOnce(&Request) -> Vec<Source>,
    ) -> bool {
        let Some(request) = request else {
            if !self.holding {
                return false;
            }
            // Nobody is searching: let go of every history read, so a closed
            // strip does not hold thousands of lines per session. Again on
            // every call until the last run has landed, since it caches the
            // terminal it was reading when it was told to stop.
            if self.running.is_some() {
                self.ticket.fetch_add(1, Ordering::Relaxed);
            } else {
                self.holding = false;
            }
            if let Ok(mut cache) = self.cache.lock() {
                cache.clear();
            }
            self.dispatched = None;
            return self.answer.take().is_some();
        };
        if let Some(running) = &self.running {
            if *running != request {
                self.ticket.fetch_add(1, Ordering::Relaxed);
            }
            return false;
        }
        if let Some(answer) = &self.answer {
            if answer.request == request {
                let printed = generation != self.generation;
                let due = self
                    .dispatched
                    .is_none_or(|at| at.elapsed() >= RESCAN_INTERVAL);
                if !(printed && due) {
                    return false;
                }
            }
        }
        self.generation = generation;
        self.dispatched = Some(Instant::now());
        self.running = Some(request.clone());
        self.holding = true;
        let sources = sources(&request);
        let tx = self.tx.clone();
        let cache = Arc::clone(&self.cache);
        let ticket = Arc::clone(&self.ticket);
        let mine = ticket.load(Ordering::Relaxed);
        std::thread::spawn(move || {
            let cancelled = || ticket.load(Ordering::Relaxed) != mine;
            let _ = tx.send(run_until(request, &sources, &cache, &cancelled));
        });
        false
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(answer) = self.rx.try_recv() {
            self.running = None;
            if answer.cancelled {
                // Due again at once: what superseded it is still waiting.
                self.dispatched = None;
                continue;
            }
            if !self.answer.as_ref().is_some_and(|held| held.same(&answer)) {
                self.serial += 1;
                self.answer = Some(Answer {
                    serial: self.serial,
                    ..answer
                });
                changed = true;
            }
        }
        changed
    }

    pub fn running(&self) -> bool {
        self.running.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> Query {
        Query::parse(text).expect("parses").expect("has terms")
    }

    #[test]
    fn a_word_matches_as_a_substring_and_ranks_above_a_subsequence() {
        let exact = q("config").match_line("load config file").unwrap();
        let fuzzy = q("cnfg").match_line("load config file").unwrap();
        assert!(exact.exact && !fuzzy.exact);
        assert!(exact.score > fuzzy.score);
        assert_eq!(exact.ranges, vec![(5, 11)]);
    }

    #[test]
    fn the_tightest_subsequence_wins_wherever_it_is() {
        // The first `c` starts a match too loose to count; a later one is tight.
        assert!(q("cnfg").match_line("c.......... config").is_some());
        assert_eq!(
            tight_subsequence(
                &"cxxxxxxxx cfg".chars().collect::<Vec<_>>(),
                &['c', 'f', 'g']
            ),
            Some(vec![10, 11, 12])
        );
    }

    #[test]
    fn a_subsequence_spread_across_a_line_is_not_a_match() {
        // Every letter is there, in order, but nowhere near each other.
        assert!(q("cfg").match_line("cat foo | grep bar").is_none());
        assert!(q("cfg").match_line("config").is_some());
    }

    #[test]
    fn every_word_must_match_in_any_order() {
        let query = q("deploy failed");
        assert!(query.match_line("failed to deploy").is_some());
        assert!(query.match_line("deploy ok").is_none());
    }

    #[test]
    fn the_whole_query_as_typed_ranks_first() {
        let query = q("deploy failed");
        let together = query.match_line("the deploy failed again").unwrap();
        let apart = query.match_line("failed: could not deploy").unwrap();
        assert!(together.score > apart.score);
    }

    #[test]
    fn case_is_ignored_until_the_query_has_a_capital() {
        assert!(q("error").match_line("ERROR: boom").is_some());
        assert!(q("Error").match_line("ERROR: boom").is_none());
        assert!(q("Error").match_line("Error: boom").is_some());
    }

    #[test]
    fn a_quoted_phrase_must_appear_verbatim() {
        let query = q("\"run the tests\"");
        assert!(query.match_line("please run the tests now").is_some());
        assert!(query.match_line("the tests run").is_none());
        // Not fuzzily, either.
        assert!(query.match_line("run thetests").is_none());
    }

    #[test]
    fn a_slashed_term_is_a_regex() {
        let query = q("/fn \\w+_test/");
        let hit = query.match_line("  fn parse_test() {").unwrap();
        assert_eq!(hit.ranges, vec![(2, 15)]);
        assert!(Query::parse("/(unclosed/").is_err());
    }

    #[test]
    fn a_standalone_word_outranks_one_inside_another() {
        let query = q("err");
        let alone = query.match_line("err: bad").unwrap();
        let inside = query.match_line("stderr bad").unwrap();
        assert!(alone.score > inside.score);
    }

    #[test]
    fn an_empty_query_has_no_terms() {
        assert!(Query::parse("   ").unwrap().is_none());
        assert!(Query::parse("\"\"").unwrap().is_none());
    }

    fn screen(rows: u16, cols: u16, input: &str) -> vt100::Parser {
        let mut parser =
            vt100::Parser::new(rows.try_into().unwrap(), cols.try_into().unwrap(), 1000);
        parser.process(input.as_bytes());
        parser
    }

    #[test]
    fn history_reads_the_scrollback_not_just_the_screen() {
        let mut text = String::from("needle here\r\n");
        for n in 0..50 {
            text.push_str(&format!("line {n}\r\n"));
        }
        let mut parser = screen(10, 40, &text);
        parser.screen_mut().set_scrollback(3);
        let history = History::read(parser.screen_mut());
        assert_eq!(history.lines[0].text, "needle here");
        assert_eq!(history.lines[0].row, 0);
        // 51 printed lines and the empty one the cursor sits on, less a screen.
        assert_eq!((history.scrollback, history.rows), (42, 10));
        // The viewport is put back where it was found.
        assert_eq!(parser.screen().scrollback(), 3);
    }

    #[test]
    fn a_wrapped_line_is_read_as_one() {
        let long = "a".repeat(30) + "NEEDLE" + &"b".repeat(10);
        let parser_text = format!("{long}\r\n");
        let mut parser = screen(5, 20, &parser_text);
        let history = History::read(parser.screen_mut());
        assert_eq!(history.lines[0].text, long);
    }

    #[test]
    fn a_hit_knows_how_to_get_back_to_it() {
        let mut text = String::from("needle\r\n");
        for n in 0..99 {
            text.push_str(&format!("{n}\r\n"));
        }
        let mut parser = screen(10, 20, &text);
        let history = History::read(parser.screen_mut());
        let hits = hits_in(&q("needle"), "s", false, &history);
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        // Scrolling to that offset puts the line on screen.
        parser.screen_mut().set_scrollback(hit.scroll);
        let visible: Vec<String> = parser.screen().rows(0, 20).collect();
        assert_eq!(visible[hit.row], "needle", "{visible:?}");
        assert!(hit.back >= 99);
    }

    #[test]
    fn a_hit_on_a_wrapped_rows_continuation_lands_on_that_row() {
        // One logical line over three rows; the needle is on the second. The
        // hit must point at the row the needle is on, not the line's first.
        let long = "a".repeat(30) + "NEEDLE" + &"b".repeat(10);
        let mut text = format!("{long}\r\n");
        for n in 0..50 {
            text.push_str(&format!("{n}\r\n"));
        }
        let mut parser = screen(5, 20, &text);
        let history = History::read(parser.screen_mut());
        let hits = hits_in(&q("needle"), "s", false, &history);
        let hit = &hits[0];
        parser.screen_mut().set_scrollback(hit.scroll);
        let visible: Vec<String> = parser.screen().rows(0, 20).collect();
        assert!(
            visible[hit.row].contains("NEEDLE"),
            "{visible:?} row {}",
            hit.row
        );
    }

    #[test]
    fn a_line_on_screen_needs_no_scrolling() {
        let mut parser = screen(10, 20, "needle\r\nnext\r\n");
        let history = History::read(parser.screen_mut());
        let hits = hits_in(&q("needle"), "s", false, &history);
        assert_eq!(hits[0].scroll, 0);
    }

    #[test]
    fn a_long_line_is_windowed_around_its_hit() {
        let line = "x".repeat(300) + " needle " + &"y".repeat(300);
        let matched = q("needle").match_line(&line).unwrap();
        let (text, ranges) = snippet(&line, &matched.ranges);
        assert!(text.starts_with('…') && text.ends_with('…'));
        assert!(text.chars().count() <= SNIPPET_CHARS + 2);
        let (s, e) = ranges[0];
        let shown: String = text.chars().skip(s).take(e - s).collect();
        assert_eq!(shown, "needle");
    }

    fn source(session: &str, stamp: u64, input: &str) -> Source {
        Source {
            session: session.into(),
            shell: false,
            parser: Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
                10.try_into().unwrap(),
                40.try_into().unwrap(),
                1000,
                crate::backend::TermSignals::default(),
            ))),
            stamp,
            restore: None,
        }
        .fed(input)
    }

    impl Source {
        fn renamed(mut self, session: &str) -> Self {
            self.session = session.into();
            self
        }

        fn fed(self, input: &str) -> Self {
            self.parser.lock().unwrap().process(input.as_bytes());
            self
        }
    }

    #[test]
    fn exact_hits_rank_first_then_the_session_that_printed_last() {
        let sources = vec![
            source("old", 1, "deploy here\r\n"),
            source("new", 2, "deploy there\r\n"),
            source("fuzzy", 3, "dploy\r\ndeplooy\r\n"),
        ];
        let answer = run(
            Request {
                query: "deploy".into(),
                sessions: None,
            },
            &sources,
            &Mutex::default(),
        );
        let order: Vec<&str> = answer.hits.iter().map(|h| h.session.as_str()).collect();
        assert_eq!(order[..2], ["new", "old"]);
        assert_eq!(answer.sessions, 3);
    }

    fn settle(store: &mut SearchStore) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while store.running() && Instant::now() < deadline {
            if store.poll() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        store.poll()
    }

    #[test]
    fn the_store_runs_a_request_once_and_reports_only_a_changed_answer() {
        let sources = vec![source("s", 1, "needle\r\n")];
        let request = Request {
            query: "needle".into(),
            sessions: None,
        };
        let mut store = SearchStore::new();
        let mut asked = 0;
        store.serve(Some(request.clone()), 1, |_| {
            asked += 1;
            sources.clone()
        });
        assert!(settle(&mut store), "the first answer is news");
        assert_eq!(store.answer().map(|a| a.hits.len()), Some(1));

        // Asked again with nothing printed: no second run.
        store.serve(Some(request.clone()), 1, |_| {
            asked += 1;
            sources.clone()
        });
        assert!(!store.running());
        assert_eq!(asked, 1);

        store.dispatched = store.dispatched.map(|at| at - RESCAN_INTERVAL);
        store.serve(Some(request.clone()), 2, |_| {
            asked += 1;
            sources.clone()
        });
        assert_eq!(asked, 2);
        assert!(!settle(&mut store), "an identical answer is not a change");

        // Asked for nothing: the answer is let go, and saying so is news.
        assert!(store.serve(None, 1, |_| Vec::new()));
        assert!(store.answer().is_none());
    }

    #[test]
    fn an_unchanged_terminal_is_read_once() {
        let sources = vec![source("s", 7, "needle\r\n")];
        let cache: Mutex<CacheMap> = Mutex::default();
        let request = Request {
            query: "needle".into(),
            sessions: None,
        };
        run(request.clone(), &sources, &cache);
        let first = Arc::clone(&cache.lock().unwrap()[&("s".to_string(), false)].1);
        run(request, &sources, &cache);
        let second = Arc::clone(&cache.lock().unwrap()[&("s".to_string(), false)].1);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn a_pane_that_could_not_be_read_back_is_tried_again() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tries = Arc::clone(&attempts);
        let restore: Restore = Arc::new(move || {
            if tries.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                return None;
            }
            let mut parser = vt100::Parser::new_with_callbacks(
                10.try_into().unwrap(),
                40.try_into().unwrap(),
                1000,
                crate::backend::TermSignals::default(),
            );
            parser.process(b"needle\r\n");
            Some(parser)
        });
        let sources = vec![Source {
            restore: Some(restore),
            ..source("s", 7, "")
        }];
        let cache: Mutex<CacheMap> = Mutex::default();
        let request = Request {
            query: "needle".into(),
            sessions: None,
        };
        let failed = run(request.clone(), &sources, &cache);
        let retried = run(request, &sources, &cache);

        assert!(failed.hits.is_empty());
        assert_eq!(retried.hits.len(), 1, "the second run read the pane back");
    }

    fn filled(rows: u16, cols: u16, scrollback: usize, lines: usize) -> Source {
        let source = Source {
            session: "s".into(),
            shell: false,
            parser: Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
                rows.try_into().unwrap(),
                cols.try_into().unwrap(),
                scrollback,
                crate::backend::TermSignals::default(),
            ))),
            stamp: 1,
            restore: None,
        };
        print_lines(&source, 0, lines, cols);
        source
    }

    fn print_lines(source: &Source, from: usize, count: usize, cols: u16) {
        let mut text = String::new();
        for n in from..from + count {
            let pad = if n % 3 == 0 { usize::from(cols) + 5 } else { 0 };
            text.push_str(&format!("line {n} {}\r\n", "x".repeat(pad)));
        }
        source.parser.lock().unwrap().process(text.as_bytes());
    }

    fn read_whole(source: &Source) -> History {
        History::read(source.parser.lock().unwrap().screen_mut())
    }

    #[test]
    fn a_read_never_holds_a_terminal_for_more_than_a_chunk() {
        // The lock is the one the terminal's reader feeds output through and
        // its paint draws from: however long one hold lasts is how long that
        // session can stall. Counted in rows, not time, so it holds on any
        // machine (ADR-P5); a row is ~1.7µs, so a chunk is well under 1ms.
        let source = filled(50, 80, 10_000, 9_000);
        let mut stats = ReadStats::default();
        let history = History::read_locked(&source.parser, None, &mut stats).unwrap();
        // A chunk, and the screen with the last of the scrollback.
        assert!(stats.widest <= CHUNK_ROWS + 50, "{stats:?}");
        assert!(stats.holds >= history.scrollback / CHUNK_ROWS, "{stats:?}");
        assert_eq!(history, read_whole(&source));
    }

    fn read_while_printing(source: &Source, between: usize, cols: u16) -> History {
        let size = screen_size(source.parser.lock().unwrap().screen());
        let mut reading = Reading::fresh(size);
        let mut printed = 1_000_000;
        loop {
            let done = reading.step(source.parser.lock().unwrap().screen_mut(), CHUNK_ROWS);
            if done {
                break;
            }
            print_lines(source, printed, between, cols);
            printed += between;
        }
        reading.finish()
    }

    #[test]
    fn a_terminal_that_prints_between_chunks_is_still_read_as_it_stands() {
        // Full scrollback: every printed line pushes the oldest out and moves
        // every row up, so a read that did not find its place again would skip
        // or repeat rows and place every hit after them wrongly.
        let source = filled(20, 60, 2_000, 3_000);
        let history = read_while_printing(&source, 7, 60);
        assert_eq!(history, read_whole(&source));

        // Not yet full: rows are added below and nothing moves.
        let source = filled(20, 60, 5_000, 1_500);
        let history = read_while_printing(&source, 7, 60);
        assert_eq!(history, read_whole(&source));
    }

    #[test]
    fn runs_of_blank_rows_do_not_pass_for_the_place_a_read_left_off() {
        // Blank rows all look alike, so a read that left off in a run of them
        // would find "its" rows unmoved however far the terminal scrolled, and
        // carry on from the wrong row.
        let source = Source {
            session: "s".into(),
            shell: false,
            parser: Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
                20.try_into().unwrap(),
                60.try_into().unwrap(),
                1_000,
                crate::backend::TermSignals::default(),
            ))),
            stamp: 1,
            restore: None,
        };
        let mut text = String::new();
        for n in 0..2_000 {
            text.push_str(&format!("line {n}\r\n"));
            if n % 5 == 0 {
                text.push_str(&"\r\n".repeat(12));
            }
        }
        source.parser.lock().unwrap().process(text.as_bytes());
        let size = screen_size(source.parser.lock().unwrap().screen());
        let mut reading = Reading::fresh(size);
        let mut printed = 0;
        loop {
            let done = reading.step(source.parser.lock().unwrap().screen_mut(), 64);
            if done {
                break;
            }
            let mut more = String::new();
            for _ in 0..3 {
                more.push_str(&format!("more {printed}\r\n"));
                printed += 1;
            }
            source.parser.lock().unwrap().process(more.as_bytes());
        }
        assert_eq!(reading.finish(), read_whole(&source));
    }

    #[test]
    fn a_rescan_reads_only_what_was_printed_since() {
        // A strip left open re-runs its query once a second while an agent
        // prints; re-reading the whole history each time kept a core busy.
        let source = filled(30, 60, 5_000, 6_000);
        let mut stats = ReadStats::default();
        let first = History::read_locked(&source.parser, None, &mut stats).unwrap();

        print_lines(&source, 6_000, 40, 60);
        let mut stats = ReadStats::default();
        let again = History::read_locked(&source.parser, Some(&first), &mut stats).unwrap();
        assert_eq!(again, read_whole(&source));
        // What was printed (some of it wrapped), the screen, and the line
        // that straddled it — not the 5,000 rows above.
        assert!(stats.rows < 40 * 2 + 30 * 2, "{stats:?}");
    }

    #[test]
    fn a_cleared_terminal_is_read_again_from_the_top() {
        let source = filled(10, 40, 1_000, 500);
        let mut stats = ReadStats::default();
        let first = History::read_locked(&source.parser, None, &mut stats).unwrap();
        // Clear the screen and the scrollback, then print something new.
        source
            .parser
            .lock()
            .unwrap()
            .process(b"\x1b[2J\x1b[3J\x1b[Hfresh start\r\n");
        let again = History::read_locked(&source.parser, Some(&first), &mut stats).unwrap();
        assert_eq!(again, read_whole(&source));
    }

    #[test]
    fn a_superseded_search_gives_up_and_says_so() {
        let sources = vec![source("s", 1, "needle\r\n")];
        let answer = run_until(
            Request {
                query: "needle".into(),
                sessions: None,
            },
            &sources,
            &Mutex::default(),
            &|| true,
        );
        assert!(answer.cancelled && answer.hits.is_empty());
    }

    #[test]
    fn an_empty_query_reads_every_history_and_matches_nothing() {
        // What an open strip asks before anything is typed, so the first
        // keystroke matches text already read.
        let sources = vec![source("a", 1, "needle\r\n"), source("b", 1, "hay\r\n")];
        let cache: Mutex<CacheMap> = Mutex::default();
        let answer = run(Request::default(), &sources, &cache);
        assert!(answer.hits.is_empty() && answer.error.is_none());
        assert_eq!(cache.lock().unwrap().len(), 2);
        assert_eq!(answer.sessions, 2);
    }

    #[test]
    fn closing_mid_run_still_lets_go_of_every_history() {
        // The run on the worker finishes the terminal it is reading after the
        // strip closes, and caches it. Clearing the cache once, at the close,
        // left that history held for as long as the strip stayed closed.
        let sources: Vec<Source> = (0..8)
            .map(|n| filled(50, 80, 10_000, 9_000).renamed(&format!("s{n}")))
            .collect();
        let mut store = SearchStore::new();
        store.serve(Some(Request::default()), 1, |_| sources.clone());
        assert!(store.running());
        // Into its first terminals, each of which takes several milliseconds;
        // the assertion below holds however the timing falls.
        std::thread::sleep(Duration::from_millis(5));
        store.serve(None, 1, |_| Vec::new());
        settle(&mut store);
        store.serve(None, 1, |_| Vec::new());
        assert!(store.cache.lock().unwrap().is_empty());
    }
}
