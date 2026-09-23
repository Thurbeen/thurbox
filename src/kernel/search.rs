//! Full-content search: every line each session's terminals still hold, not just
//! the screen they are showing.
//!
//! The search strip used to be handed each session's *visible* screen, so a
//! prompt typed a few minutes earlier — scrolled off, but still in the vt100
//! scrollback — could not be found. This reads the scrollback too, and because
//! that is thousands of lines per session it runs on a worker: the loop hands
//! over the parsers (an `Arc` clone each) and gets ranked hits back a frame or
//! two later, the same shape as [`super::diff`]. Nothing here runs on the
//! render thread except [`SearchStore::serve`], which only compares a request
//! against the one it last answered.
//!
//! A hit says where it is in terms the agent pane can act on: how many rows
//! from the bottom of the history the line sits (`back`) and the scrollback
//! offset that puts it on screen (`scroll`), so opening a result lands on it
//! rather than merely focusing the session.
//!
//! Matching, in [`Query`]: every term must hit the same line; a word matches as
//! a substring or, failing that, as a tight subsequence, and exact hits rank
//! above fuzzy ones; `"a phrase"` must appear verbatim; `/re/` is a regular
//! expression; and a query with no capital letters ignores case.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The key a plugin leaves in `store` to ask for a content search.
///
/// A parameterised read, like the creation flow's repository questions: nobody
/// wants every agent's history read on every frame, so it is served only while
/// something is asking. Its value is the query, which is also what makes
/// "asking" and "having a query" the same state.
pub const WANT_CONTENT: &str = "want_content";

/// Optional companion to [`WANT_CONTENT`]: space-separated session ids to limit
/// the search to. Absent searches every session.
pub const WANT_SESSIONS: &str = "want_content.sessions";

/// Most hits kept for one session, best first. A one-letter query matches most
/// lines of every session; the list is for picking one, not for reading them all.
pub const HITS_PER_SESSION: usize = 50;

/// Most hits published in total, across every session.
pub const MAX_HITS: usize = 200;

/// Longest context line handed to a pane, in characters. The line is windowed
/// around its first hit, so a match at column 300 of a long line still shows.
pub const SNIPPET_CHARS: usize = 160;

/// Characters of lead-in kept before the first hit when a line is windowed.
const SNIPPET_LEAD: usize = 24;

/// How often an unchanged query is re-run because a terminal printed.
///
/// A query change is served at once. Output alone is not: an agent mid-turn
/// prints on every frame, and re-reading its history that often would keep a
/// core busy for as long as the strip is open, to move a result list nobody is
/// reading line by line.
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(1);

// ── Query ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Term {
    /// Substring first, tight subsequence second. Stored case-folded when the
    /// query is case-insensitive.
    Word(String),
    /// Verbatim, as a substring only.
    Phrase(String),
    Regex(regex::Regex),
}

/// A parsed query: the terms every matching line must contain.
#[derive(Debug, Clone)]
pub struct Query {
    terms: Vec<Term>,
    case_sensitive: bool,
    /// The plain words joined by single spaces, for the bonus a line earns by
    /// containing the whole query as typed. `None` for a single term.
    whole: Option<String>,
}

/// Case-fold one character, keeping it one character so positions line up
/// with the original. `İ` lowercases to two; its first is close enough.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// A line case-folded character by character — the same count of characters
/// as the original, so a character index means the same place in both.
pub fn fold_line(line: &str) -> String {
    if line.is_ascii() {
        return line.to_ascii_lowercase();
    }
    line.chars().map(fold).collect()
}

impl Query {
    /// Parse the search grammar. `Ok(None)` for a query with no terms.
    ///
    /// * words, separated by spaces — each must match, in any order
    /// * `"exact phrase"` — must appear verbatim (an unclosed quote runs to the
    ///   end)
    /// * `/regex/` — a regular expression (Rust `regex` syntax)
    /// * smart case: any capital letter makes the whole query case-sensitive
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

    /// Match one line: the score and the character ranges that hit, or `None`
    /// when any term is missing.
    pub fn match_line(&self, line: &str) -> Option<LineMatch> {
        if self.case_sensitive {
            self.match_folded(line, line)
        } else {
            self.match_folded(line, &fold_line(line))
        }
    }

    /// [`Self::match_line`] with the line already folded by [`fold_line`] —
    /// what a [`History`] keeps, so a query typed a letter at a time folds each
    /// line once rather than once per keystroke. Ignored when the query is
    /// case-sensitive.
    ///
    /// Substrings are found with `str::find` on the folded text, and only a
    /// word that misses as a substring, and passes a cheap in-order check,
    /// pays for the subsequence search.
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

/// Where a `/regex/` term that opens `text` closes: the first unescaped `/`
/// followed by a space or the end. A pattern may hold spaces — `/fn \w+ test/`
/// — and a lone `/` or `//` is a word, not an empty pattern.
fn regex_end(text: &str) -> Option<usize> {
    let body = text.strip_prefix('/')?;
    let mut escaped = false;
    for (at, c) in body.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '/' if !escaped => {
                let after = &body[at + 1..];
                if at > 0 && after.chars().next().map_or(true, char::is_whitespace) {
                    return Some(at + 1);
                }
            }
            _ => escaped = false,
        }
    }
    None
}

/// A matched line, before it is placed in a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
    pub score: i32,
    /// Every term matched as a substring (or phrase, or regex).
    pub exact: bool,
    /// Character ranges `[start, end)` that hit, sorted and merged.
    pub ranges: Vec<(usize, usize)>,
}

/// Where `needle` first occurs in `haystack`, as a character range, with the
/// bonus for standing alone: `err` in `err: …` above `err` in `stderr`.
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
        .map_or(true, |c| !c.is_alphanumeric());
    let close = after.chars().next().map_or(true, |c| !c.is_alphanumeric());
    let bonus = match (open, close) {
        (true, true) => 30,
        (true, false) | (false, true) => 15,
        _ => 0,
    };
    let at = before.chars().count();
    Some((at, at + needle.chars().count(), bonus))
}

/// Whether every character of `needle` appears in `haystack` in order — the
/// allocation-free check that turns most lines away before the subsequence
/// search has to build anything.
fn in_order(haystack: &str, needle: &str) -> bool {
    let mut wanted = needle.chars().peekable();
    for c in haystack.chars() {
        if wanted.peek() == Some(&c) {
            wanted.next();
        }
        if wanted.peek().is_none() {
            return true;
        }
    }
    wanted.peek().is_none()
}

/// The subsequence of `needle` in `haystack` with the shortest span, if that
/// span is at most twice the needle's length.
///
/// Bounded because a subsequence anywhere in a long terminal line is noise: a
/// three-letter word is a subsequence of most lines on a screen. Within twice
/// its length it is a word with letters missing — `cnfg` for `config` — which is
/// the case subsequence matching is for.
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
        if positions.len() == needle.len() && best.as_ref().map_or(true, |(b, _)| span < *b) {
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

/// One logical line of a terminal: its rows joined where the terminal wrapped
/// them, so a long command found on its second row is found at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryLine {
    pub text: String,
    /// `text` through [`fold_line`], kept so matching folds nothing.
    pub folded: String,
    /// The row the line starts on, counted from the oldest scrollback row.
    pub row: usize,
}

/// Everything one terminal still holds, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub lines: Vec<HistoryLine>,
    /// Rows in the scrollback, above the screen.
    pub scrollback: usize,
    /// Rows on the screen.
    pub rows: usize,
    /// Size the screen was read at — a resize changes what the rows hold
    /// without anything being printed.
    pub size: (u16, u16),
}

impl History {
    /// Read a screen's scrollback and visible rows.
    ///
    /// vt100 exposes scrollback only through the viewport, so this pages the
    /// offset up to the top and back down a screen at a time, then puts it back
    /// where it was — under the caller's lock, so no reader sees it moved. Both
    /// `set_scrollback` and `scrollback` are O(1) in vt100 0.16.
    ///
    /// On the alternate screen (a full-screen program) there is no scrollback
    /// to read: vt100 keeps none for it, and this reads the screen alone.
    pub fn read(screen: &mut vt100::Screen) -> Self {
        let size = screen.size();
        let (rows, cols) = (usize::from(size.0), size.1);
        let offset = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let scrollback = screen.scrollback();
        let total = scrollback + rows;

        let mut lines: Vec<HistoryLine> = Vec::new();
        let mut continues = false;
        let mut start = 0;
        while start < total && rows > 0 {
            // At offset `k` the viewport's first row is history row
            // `scrollback - k`.
            let k = scrollback.saturating_sub(start);
            screen.set_scrollback(k);
            let base = scrollback - k;
            let first = start - base;
            let last = rows.min(total - base);
            for (r, text) in screen.rows(0, cols).enumerate().take(last).skip(first) {
                let row = base + r;
                match lines.last_mut() {
                    Some(line) if continues => line.text.push_str(&text),
                    _ => lines.push(HistoryLine {
                        text,
                        folded: String::new(),
                        row,
                    }),
                }
                continues = u16::try_from(r).is_ok_and(|r| screen.row_wrapped(r));
            }
            start = base + last;
        }
        screen.set_scrollback(offset);
        for line in &mut lines {
            line.folded = fold_line(&line.text);
        }
        Self {
            lines,
            scrollback,
            rows,
            size,
        }
    }

    /// Rows from the bottom of the history to `row`: 0 is the last row on the
    /// screen.
    pub fn back(&self, row: usize) -> usize {
        (self.scrollback + self.rows).saturating_sub(row + 1)
    }

    /// The screen row `row` lands on once the view is scrolled to
    /// [`Self::scroll_to`] it.
    pub fn row_on_screen(&self, row: usize) -> usize {
        (row + self.scroll_to(row)).saturating_sub(self.scrollback)
    }

    /// The scrollback offset that shows `row` a third of the way down the
    /// screen — context above it, and room below for what followed. A row
    /// already on the screen needs no scrolling at all.
    pub fn scroll_to(&self, row: usize) -> usize {
        if row >= self.scrollback {
            return 0;
        }
        (self.scrollback + self.rows / 3)
            .saturating_sub(row)
            .min(self.scrollback)
    }
}

// ── Hits ────────────────────────────────────────────────────────────────────

/// One matching line, located.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub session: String,
    /// Found in the session's companion shell rather than its agent.
    pub shell: bool,
    /// The line, trimmed and windowed to [`SNIPPET_CHARS`] around the hit.
    pub text: String,
    /// Character ranges `[start, end)` of `text` that matched.
    pub ranges: Vec<(usize, usize)>,
    /// Rows between the line and the bottom of its terminal.
    pub back: usize,
    /// The scrollback offset that puts the line on screen.
    pub scroll: usize,
    /// The screen row the line is on once scrolled to `scroll`, from the top.
    ///
    /// Counted from the top rather than the bottom because that is what a
    /// resize leaves alone: the pane grows when the search strip closes, and
    /// vt100 adds the new rows at the bottom.
    pub row: usize,
    pub exact: bool,
    pub score: i32,
}

/// The line as a pane shows it: trimmed, and cut to a window that starts just
/// before the first hit, with the ranges moved to match.
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

/// A matching line, before it is turned into a [`Hit`]: where it is and how
/// well it matched. Kept this small because a one-letter query matches most
/// lines of every session, and all but a few hundred are ranked away — the
/// snippet is built only for those that survive.
struct Found {
    source: usize,
    line: usize,
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
            scroll: history.scroll_to(line.row),
            row: history.row_on_screen(line.row),
            exact: self.matched.exact,
            score: self.matched.score,
        }
    }
}

/// Every line of one terminal's history that matches `query`.
fn found_in(query: &Query, source: usize, history: &History) -> Vec<Found> {
    history
        .lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            Some(Found {
                source,
                line: index,
                matched: query.match_folded(&line.text, &line.folded)?,
                back: history.back(line.row),
            })
        })
        .collect()
}

/// Every hit for `query` in one terminal's history, in history order.
#[cfg(test)]
fn hits_in(query: &Query, session: &str, shell: bool, history: &History) -> Vec<Hit> {
    let histories = vec![Arc::new(history.clone())];
    let sources = vec![Source {
        session: session.into(),
        shell,
        parser: Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            1,
            1,
            0,
            crate::agent::TermSignals::default(),
        ))),
        stamp: 0,
    }];
    found_in(query, 0, history)
        .into_iter()
        .map(|found| found.hit(&sources, &histories))
        .collect()
}

// ── The worker ──────────────────────────────────────────────────────────────

/// One terminal to search: a session's agent or its shell.
#[derive(Clone)]
pub struct Source {
    pub session: String,
    pub shell: bool,
    pub parser: Arc<Mutex<crate::agent::SessionParser>>,
    /// When the pane last printed, in epoch milliseconds — both the cache key
    /// for its history and the recency a hit is ranked by.
    pub stamp: u64,
}

/// What a plugin asked for: the query text and, optionally, which sessions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Request {
    pub query: String,
    /// `None` searches every session.
    pub sessions: Option<Vec<String>>,
}

/// A finished search.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    pub request: Request,
    /// Ranked hits, at most [`MAX_HITS`].
    pub hits: Vec<Hit>,
    /// Matching lines found, before any cap.
    pub total: usize,
    /// Sessions whose terminals were read.
    pub sessions: usize,
    /// Lines searched, across every terminal.
    pub lines: usize,
    /// Wall time the worker spent, reading and matching.
    pub elapsed: Duration,
    /// Why the query could not run — an invalid regex.
    pub error: Option<String>,
}

impl Answer {
    /// The same answer, timing aside — what a pane would draw is unchanged.
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

/// Run one search over `sources`. Called on a worker thread; public so a
/// benchmark can time exactly what the worker does.
pub fn run(request: Request, sources: &[Source], cache: &Mutex<CacheMap>) -> Answer {
    let started = Instant::now();
    let query = match Query::parse(&request.query) {
        Ok(Some(query)) => query,
        Ok(None) => {
            return Answer {
                request,
                ..Answer::default()
            }
        }
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

    let mut histories: Vec<Arc<History>> = Vec::with_capacity(sources.len());
    let mut per_session: HashMap<&str, Vec<Found>> = HashMap::new();
    let mut lines = 0;
    let mut total = 0;
    for (index, source) in sources.iter().enumerate() {
        let key = (source.session.clone(), source.shell);
        let history = history_of(source, cache.lock().ok().and_then(|c| c.get(&key).cloned()));
        if let Ok(mut cache) = cache.lock() {
            cache.insert(key, (source.stamp, Arc::clone(&history)));
        }
        lines += history.lines.len();
        let found = found_in(&query, index, &history);
        total += found.len();
        per_session
            .entry(source.session.as_str())
            .or_default()
            .extend(found);
        histories.push(history);
    }

    let searched = per_session.len();
    let mut ranked: Vec<Found> = Vec::new();
    for (_, mut found) in per_session {
        found.sort_by(|a, b| rank(&a.matched, a.back, &b.matched, b.back));
        found.truncate(HITS_PER_SESSION);
        ranked.extend(found);
    }
    let session_of = |found: &Found| sources[found.source].session.as_str();
    ranked.sort_by(|a, b| {
        rank(&a.matched, 0, &b.matched, 0)
            .then_with(|| recency.get(session_of(b)).cmp(&recency.get(session_of(a))))
            .then(a.back.cmp(&b.back))
            .then(session_of(a).cmp(session_of(b)))
    });
    ranked.truncate(MAX_HITS);
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
        error: None,
    }
}

/// Best match first — every term exact before any fuzzy, then score — and the
/// most recent (fewest rows back) of equals.
fn rank(a: &LineMatch, a_back: usize, b: &LineMatch, b_back: usize) -> std::cmp::Ordering {
    b.exact
        .cmp(&a.exact)
        .then(b.score.cmp(&a.score))
        .then(a_back.cmp(&b_back))
}

/// The cache a [`SearchStore`] keeps between runs: each terminal's history,
/// keyed by session and pane, with the output stamp it was read at.
pub type CacheMap = HashMap<(String, bool), (u64, Arc<History>)>;

/// A terminal's history: the cached read while the pane has printed nothing
/// since and its size has not changed, otherwise a fresh one.
fn history_of(source: &Source, cached: Option<(u64, Arc<History>)>) -> Arc<History> {
    let Ok(mut parser) = source.parser.lock() else {
        return cached.map(|(_, h)| h).unwrap_or_default();
    };
    let screen = parser.screen_mut();
    if let Some((stamp, history)) = cached {
        if stamp == source.stamp && history.size == screen.size() {
            return history;
        }
    }
    Arc::new(History::read(screen))
}

/// Serves search requests on a worker, one at a time, newest wins.
pub struct SearchStore {
    tx: Sender<Answer>,
    rx: Receiver<Answer>,
    cache: Cache,
    /// The request on the worker right now.
    running: Option<Request>,
    answer: Option<Answer>,
    /// When the answer being held was dispatched, for [`RESCAN_INTERVAL`].
    dispatched: Option<Instant>,
    /// The output generation the held answer was read at.
    generation: u64,
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
            answer: None,
            dispatched: None,
            generation: 0,
        }
    }

    /// The last finished search, if one is being held.
    pub fn answer(&self) -> Option<&Answer> {
        self.answer.as_ref()
    }

    /// Ask for `request`, or for nothing. Returns true when the held answer was
    /// dropped, so the caller can republish.
    ///
    /// Cheap on the loop: it compares, and only when a run is due does it call
    /// `sources` — which clones one `Arc` per terminal — and spawn. A run
    /// already on the worker is left to finish; the next call after it lands
    /// dispatches whatever is being asked for by then.
    pub fn serve(
        &mut self,
        request: Option<Request>,
        generation: u64,
        sources: impl FnOnce(&Request) -> Vec<Source>,
    ) -> bool {
        let Some(request) = request else {
            // Nobody is searching: let go of every history read, so a closed
            // strip does not hold thousands of lines per session.
            if let Ok(mut cache) = self.cache.lock() {
                cache.clear();
            }
            self.dispatched = None;
            return self.answer.take().is_some();
        };
        if self.running.is_some() {
            return false;
        }
        if let Some(answer) = &self.answer {
            if answer.request == request {
                let printed = generation != self.generation;
                let due = self
                    .dispatched
                    .map_or(true, |at| at.elapsed() >= RESCAN_INTERVAL);
                if !(printed && due) {
                    return false;
                }
            }
        }
        self.generation = generation;
        self.dispatched = Some(Instant::now());
        self.running = Some(request.clone());
        let sources = sources(&request);
        let tx = self.tx.clone();
        let cache = Arc::clone(&self.cache);
        std::thread::spawn(move || {
            let _ = tx.send(run(request, &sources, &cache));
        });
        false
    }

    /// Fold a finished run in. True only when the answer changed: a re-run
    /// on output that found the same lines moves nothing a pane reads, and
    /// moving the data epoch for it would drop every pure pane's cached tree
    /// once a second while an agent prints.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(answer) = self.rx.try_recv() {
            self.running = None;
            if !self.answer.as_ref().is_some_and(|held| held.same(&answer)) {
                self.answer = Some(answer);
                changed = true;
            }
        }
        changed
    }

    /// Whether a run is on the worker.
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
        let mut parser = vt100::Parser::new(rows, cols, 1000);
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
                10,
                40,
                1000,
                crate::agent::TermSignals::default(),
            ))),
            stamp,
        }
        .fed(input)
    }

    impl Source {
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

    /// Wait for the worker, the way the loop's next iteration would.
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

        // Something printed and the pacing interval passed: it runs again, and
        // finding the same lines is not news — no epoch moves for it.
        std::thread::sleep(RESCAN_INTERVAL);
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
}
