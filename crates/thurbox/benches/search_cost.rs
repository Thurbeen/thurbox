use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thurbox::backend::{SessionParser, TermSignals};
use thurbox::kernel::search::{run, CacheMap, History, ReadStats, Request, Source};

fn env(name: &str, fallback: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}

const LOCK_BUDGET: Duration = Duration::from_millis(1);

const KEYSTROKE_BUDGET: Duration = Duration::from_millis(50);

const RESCAN_BUDGET: Duration = Duration::from_millis(50);

const ROWS: u16 = 50;
const COLS: u16 = 200;

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
            let mut parser = vt100::Parser::new_with_callbacks(
                ROWS.try_into().unwrap(),
                COLS.try_into().unwrap(),
                scrollback,
                TermSignals::default(),
            );
            fill(&mut parser, scrollback + usize::from(ROWS), n);
            Source {
                session: format!("session-{n}"),
                shell: false,
                parser: Arc::new(Mutex::new(parser)),
                stamp: n as u64,
                restore: None,
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

fn request(query: &str) -> Request {
    Request {
        query: query.into(),
        sessions: None,
    }
}

fn main() {
    let sessions = env("THURBOX_BENCH_SESSIONS", 20);
    let scrollback = env("THURBOX_BENCH_SCROLLBACK", 1000);
    let runs = env("THURBOX_BENCH_RUNS", 9);
    let check = std::env::var_os("THURBOX_BENCH_CHECK").is_some();
    let terminals = sources(sessions, scrollback);
    let mut over: Vec<String> = Vec::new();
    let mut budget = |what: &str, took: Duration, limit: Duration| {
        if took > limit {
            over.push(format!("{what} {} > {}", ms(took), ms(limit)));
        }
    };

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
    println!(
        "  loop    (hand the worker its sources)   {}",
        ms(loop_cost)
    );

    let mut read = ReadStats::default();
    for source in &terminals {
        std::hint::black_box(History::read_locked(&source.parser, None, &mut read));
    }
    let whole = terminals
        .iter()
        .map(|source| {
            let mut parser = source.parser.lock().unwrap();
            let started = Instant::now();
            std::hint::black_box(History::read(parser.screen_mut()));
            started.elapsed()
        })
        .max()
        .unwrap_or_default();
    println!(
        "  lock    (longest one parser is held)    {}  ({} rows at most; {} to read a history whole)",
        ms(read.held),
        read.widest,
        ms(whole)
    );
    budget("lock", read.held, LOCK_BUDGET);

    let cold = median(
        (0..runs)
            .map(|_| {
                let cache: Mutex<CacheMap> = Mutex::default();
                let started = Instant::now();
                std::hint::black_box(run(request(""), &terminals, &cache));
                started.elapsed()
            })
            .collect(),
    );
    println!("  cold    (read every history)            {}", ms(cold));

    let cache: Mutex<CacheMap> = Mutex::default();
    run(request(""), &terminals, &cache);
    for query in [
        "flaky login",
        "login flaky",
        "e",
        "cmpile",
        "zzqx",
        "\"login test\"",
        "/fa\\w+ed/",
    ] {
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
            "  {query:<14}  warm {:>9}  {} lines, {} matching, {} shown",
            ms(warm),
            answer.lines,
            answer.total,
            answer.hits.len()
        );
        budget(&format!("warm {query}"), warm, KEYSTROKE_BUDGET);
    }

    let typed = "compile error";
    let slowest = (1..=typed.len())
        .map(|n| {
            median(
                (0..runs)
                    .map(|_| {
                        let started = Instant::now();
                        std::hint::black_box(run(request(&typed[..n]), &terminals, &cache));
                        started.elapsed()
                    })
                    .collect(),
            )
        })
        .max()
        .unwrap_or_default();
    println!("  typing  (slowest letter of {typed:?})  {}", ms(slowest));
    budget("typing", slowest, KEYSTROKE_BUDGET);

    // Three agents print a burst, and the open strip re-runs its query.
    let mut rescans = Vec::new();
    let mut rows = 0;
    for round in 0..runs {
        let printed: Vec<Source> = terminals
            .iter()
            .enumerate()
            .map(|(n, source)| {
                let mut source = source.clone();
                if n < 3 {
                    fill(&mut source.parser.lock().unwrap(), 30, 1000 + round);
                    source.stamp += 1000 + round as u64;
                }
                source
            })
            .collect();
        let started = Instant::now();
        let answer = run(request("flaky login"), &printed, &cache);
        rescans.push(started.elapsed());
        rows = answer.read.rows;
    }
    let rescan = median(rescans);
    println!(
        "  rescan  (3 agents printed 30 lines)     {}  ({rows} rows read again)",
        ms(rescan)
    );
    budget("rescan", rescan, RESCAN_BUDGET);

    if check {
        if over.is_empty() {
            println!("  within budget");
        } else {
            for line in &over {
                println!("  OVER BUDGET: {line}");
            }
            std::process::exit(1);
        }
    }
}
