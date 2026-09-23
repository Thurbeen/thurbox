//! What a content search costs, over every session's full scrollback.
//!
//! Not a gate — ADR-P5 keeps timing out of CI; `kernel::search`'s unit tests
//! hold the assertions that are deterministic. This is the instrument: it fills
//! real vt100 parsers with agent-shaped output and times the three things the
//! search does, so the numbers in `docs/PERFORMANCE.md` can be re-measured
//! rather than trusted.
//!
//! ```sh
//! cargo bench --bench search_cost                          # 20 sessions × 1000
//! THURBOX_BENCH_SESSIONS=20 THURBOX_BENCH_SCROLLBACK=10000 cargo bench --bench search_cost
//! ```
//!
//! * **loop** — what the render thread pays to start a search: one `Source`
//!   (an `Arc` clone and a stamp) per terminal. The rest is on the worker.
//! * **cold** — the worker's first run: every history read under its parser's
//!   lock, then matched.
//! * **warm** — a new query over terminals that printed nothing since: the
//!   histories come from the cache, so this is matching alone. It is what each
//!   keystroke costs once the debounce lets it through.
//! * **lock** — the longest one parser was held while its history was read,
//!   which is how long that session's reader thread (and its paint) could wait.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thurbox::agent::{SessionParser, TermSignals};
use thurbox::kernel::search::{run, CacheMap, History, Request, Source};

fn env(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}

const ROWS: u16 = 50;
const COLS: u16 = 200;

/// Agent-shaped output: prose, paths, code and blank lines, deterministic so
/// every run reads the same text. A line in forty mentions the needle, the way
/// a word you search for appears a handful of times in a long session.
fn fill(parser: &mut SessionParser, lines: usize, seed: usize) {
    const WORDS: [&str; 24] = [
        "the",
        "session",
        "worktree",
        "branch",
        "failed",
        "compile",
        "error",
        "src/main.rs",
        "fn",
        "let",
        "tests",
        "passed",
        "running",
        "cargo",
        "warning:",
        "unused",
        "variable",
        "match",
        "=>",
        "Ok(())",
        "diff",
        "--git",
        "a/src/lib.rs",
        "b/src/lib.rs",
    ];
    let mut state = seed.wrapping_mul(2654435761).wrapping_add(1);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut out = String::new();
    for n in 0..lines {
        if n % 11 == 0 {
            out.push_str("\r\n");
            continue;
        }
        let words = 4 + next() % 24;
        for w in 0..words {
            if w > 0 {
                out.push(' ');
            }
            out.push_str(WORDS[next() % WORDS.len()]);
        }
        if n % 40 == 7 {
            out.push_str(" flaky login test");
        }
        out.push_str("\r\n");
    }
    parser.process(out.as_bytes());
}

fn sources(sessions: usize, scrollback: usize) -> Vec<Source> {
    (0..sessions)
        .map(|n| {
            let mut parser =
                vt100::Parser::new_with_callbacks(ROWS, COLS, scrollback, TermSignals::default());
            fill(&mut parser, scrollback + usize::from(ROWS), n);
            Source {
                session: format!("session-{n}"),
                shell: false,
                parser: Arc::new(Mutex::new(parser)),
                stamp: n as u64,
            }
        })
        .collect()
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.2}ms", d.as_secs_f64() * 1000.0)
}

fn main() {
    let sessions = env("THURBOX_BENCH_SESSIONS", 20);
    let scrollback = env("THURBOX_BENCH_SCROLLBACK", 1000);
    let runs = env("THURBOX_BENCH_RUNS", 9);
    let terminals = sources(sessions, scrollback);

    println!(
        "content search: {sessions} sessions × {scrollback} scrollback rows ({ROWS}×{COLS} screens), \
         median of {runs}"
    );

    let loop_cost = median(
        (0..runs)
            .map(|_| {
                let started = Instant::now();
                let handed: Vec<Source> = terminals.to_vec();
                std::hint::black_box(&handed);
                started.elapsed()
            })
            .collect(),
    );
    println!("  loop  (hand the worker its sources)   {}", ms(loop_cost));

    let lock = terminals
        .iter()
        .map(|source| {
            let mut parser = source.parser.lock().unwrap();
            let started = Instant::now();
            std::hint::black_box(History::read(parser.screen_mut()));
            started.elapsed()
        })
        .max()
        .unwrap_or_default();
    println!("  lock  (longest one parser is held)    {}", ms(lock));

    for query in [
        "flaky login",
        "login flaky",
        "e",
        "cmpile",
        "\"login test\"",
        "/fa\\w+ed/",
    ] {
        let request = |q: &str| Request {
            query: q.into(),
            sessions: None,
        };
        let cold = median(
            (0..runs)
                .map(|_| {
                    let cache: Mutex<CacheMap> = Mutex::default();
                    let started = Instant::now();
                    std::hint::black_box(run(request(query), &terminals, &cache));
                    started.elapsed()
                })
                .collect(),
        );
        let cache: Mutex<CacheMap> = Mutex::default();
        run(request("warm-up"), &terminals, &cache);
        let mut answer = None;
        let warm = median(
            (0..runs)
                .map(|_| {
                    let started = Instant::now();
                    answer = Some(run(request(query), &terminals, &cache));
                    started.elapsed()
                })
                .collect(),
        );
        let answer = answer.expect("ran");
        println!(
            "  {query:<14} cold {:>9}  warm {:>9}  {} lines, {} matching, {} shown",
            ms(cold),
            ms(warm),
            answer.lines,
            answer.total,
            answer.hits.len()
        );
    }
}
