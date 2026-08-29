//! Architecture rules enforced as tests (allowlist model).
//!
//! Every module under `src/` must appear in [`MODULE_RULES`] (or in
//! [`EXEMPT`]) and may only reference the crate-internal modules its entry
//! allows — `every_module_is_governed` fails when a new module is added
//! without a rule, so the architecture is an explicit decision per module.
//!
//! References are extracted from comment- and string-stripped source, so all
//! import shapes are covered: `use` / `pub use`, brace groups
//! (`use crate::{a, b}`), bare imports (`use crate::a;`), multi-line
//! statements, and fully-qualified paths in code (`crate::a::item(…)`).
//!
//! The layering mirrors CLAUDE.md ("Module Dependency Rules") and
//! docs/CONSTITUTION.md §2. If a rule change is intentional, update those
//! docs in the same PR.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Per-module dependency allowlist.
struct ModuleRules {
    /// Module name: a directory `src/<name>/` or a file `src/<name>.rs`.
    name: &'static str,
    /// Crate modules this module may reference in any form.
    allowed: &'static [&'static str],
    /// Crate modules additionally reachable via fully-qualified paths
    /// (`crate::module::item(…)`) but **not** importable with `use` —
    /// keeps the dependency visible at every call site.
    allowed_path_only: &'static [&'static str],
}

/// Which crate-internal modules each module may touch.
const MODULE_RULES: &[ModuleRules] = &[
    // Pure data: the dependency sink. No crate-internal references at all.
    ModuleRules {
        name: "session",
        allowed: &[],
        allowed_path_only: &[],
    },
    // Side-effect layer (PTY/tmux). Never ui, git, or app.
    ModuleRules {
        name: "agent",
        allowed: &["session", "paths", "shell"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "git",
        allowed: &["session", "paths", "shell"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "storage",
        allowed: &["session", "sync", "paths"],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "sync",
        allowed: &["session", "storage", "workspace"],
        allowed_path_only: &[],
    },
    // `shell` builds the host launchers (ssh/wsl) for reading a remote
    // session's credentials where the agent actually runs.
    ModuleRules {
        name: "usage",
        allowed: &["session", "shell"],
        // `paths::home_dir()` (fully-qualified, never `use`) to resolve agent
        // credential files cross-platform ($HOME / %USERPROFILE%).
        allowed_path_only: &["paths"],
    },
    // Headless session ops: no TUI state or PTY-attached backend. Talks to
    // tmux through the narrow helpers in `agent::tmux` via fully-qualified
    // paths only (never `use`), same pattern as the cli module. `shell` for
    // the same reason `agent` has it: `host_cli` spells a `thurbox-cli`
    // invocation for a host's `sh` or PowerShell, and the two quoting rules
    // have exactly one home (`shell::posix_quote` / `powershell_quote`).
    ModuleRules {
        name: "session_ops",
        allowed: &[
            "session",
            "storage",
            "git",
            "sync",
            "paths",
            "workspace",
            "shell",
        ],
        allowed_path_only: &["agent"],
    },
    // Thin headless dispatch — must not depend on TUI or the live backend.
    ModuleRules {
        name: "cli",
        allowed: &[
            "session",
            "storage",
            "session_ops",
            "sync",
            "paths",
            "notifications",
        ],
        // `kernel` for `plugin check`, which loads the *real* host: the failures
        // worth reporting are declaration-shaped (no `render`, an unplaced slot, a
        // clashing key) and a syntax check passes all of them. Path-only, like
        // `agent`, so the crossing stays visible at each call site.
        allowed_path_only: &["agent", "kernel"],
    },
    // v2 plugin kernel: hosts the Lua VM the whole UI is written in. Reads the
    // session engine to build the snapshot plugins render from (`storage` +
    // `sync` for the rows, `session` for the types, `paths` for the DB and
    // plugin directories). Never `ui` or `app` — it is their replacement, not
    // their peer.
    //
    // `git` IS allowed, and the rule is about *where*: the worker-backed stores
    // (`diff`, `repos`, `packages`, `command`) shell out to it off-thread, which
    // is rule 5 rather than an exception to it. What must not happen is a `git`
    // call from a render path — that is enforced by the loop's shape (a plugin
    // returns a tree; it cannot call Rust), not by this allowlist.
    ModuleRules {
        name: "kernel",
        allowed: &[
            "session",
            "storage",
            "sync",
            "paths",
            "session_ops",
            "git",
            "notifications",
            "clipboard",
        ],
        // Live agent terminals: `kernel::terminal` adopts a session's real pane
        // and paints its vt100 screen. `kernel::metrics` fetches account usage
        // through `usage`. Both are reachable by fully-qualified path only
        // (never `use`), the same rule `session_ops` and `cli` follow, so every
        // crossing into the side-effect layer is visible at its call site.
        allowed_path_only: &["agent", "usage"],
    },
    // Leaf utilities.
    ModuleRules {
        name: "paths",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "shell",
        allowed: &[],
        allowed_path_only: &[],
    },
    ModuleRules {
        name: "workspace",
        allowed: &["paths"],
        allowed_path_only: &[],
    },
    // Leaf side-effect module: OS desktop notifications. Knows about
    // `session` (for `SessionId`), `paths` (for the DB path the click
    // callback writes to) and `shell` (the shared quoting rules — it grew a
    // third copy of the PowerShell one before this was allowed); never
    // reaches into agent / ui / app / storage beyond a single SQL statement
    // on its own short-lived connection.
    ModuleRules {
        name: "notifications",
        allowed: &["session", "paths", "shell"],
        allowed_path_only: &[],
    },
    // Leaf side-effect module: clipboard writes (native + OSC 52). Knows
    // `session` only for the `ClipboardProvider` setting; writes to the tty
    // and never reaches into agent / ui / app / storage.
    ModuleRules {
        name: "clipboard",
        allowed: &["session"],
        allowed_path_only: &[],
    },
];

/// Modules exempt from the allowlist: `app` is the coordinator (imports
/// everything by design); `bin`, `lib`, and `main` are crate roots, not
/// architecture modules.
// `main` is the coordinator, as `app` was before v1 was retired.
/// `coordinator` is `main`'s own body, split across files: it wires every
/// layer together by definition, which is exactly why `main` is exempt.
const EXEMPT: &[&str] = &["bin", "coordinator", "lib", "main"];

/// A single architecture violation: a forbidden crate-module reference.
struct Violation {
    file: PathBuf,
    line_number: usize,
    line: String,
    segment: String,
    in_use: bool,
}

/// A `crate::<segment>` reference found in stripped source.
struct RefSite {
    /// Byte offset of the `crate::` token (for line lookup).
    offset: usize,
    segment: String,
    /// Whether the reference sits inside a `use …;` statement.
    in_use: bool,
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Recursively collect all `.rs` files under `dir`, panicking on I/O errors
/// so a renamed or unreadable module can never pass vacuously.
fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    collect_files_with_extension(dir, "rs")
}

/// The `.rs` files making up module `name` (`src/<name>/` or `src/<name>.rs`).
fn module_files(name: &str) -> Vec<PathBuf> {
    let root = src_root();
    let dir = root.join(name);
    if dir.is_dir() {
        let files = collect_rs_files(&dir);
        assert!(!files.is_empty(), "src/{name}/ contains no .rs files");
        return files;
    }
    let file = root.join(format!("{name}.rs"));
    assert!(
        file.is_file(),
        "module `{name}` not found as src/{name}/ or src/{name}.rs — \
         update MODULE_RULES in tests/architecture_rules.rs if it was renamed"
    );
    vec![file]
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Strip comments and string/char-literal contents from Rust source,
/// preserving newlines so byte offsets still map to line numbers.
///
/// One `skip_*` helper per lexical form, each returning the index just past what
/// it consumed and pushing only the newlines it swallowed.
fn strip_comments_and_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        i = match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => skip_line_comment(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => skip_block_comment(bytes, i, &mut out),
            b'"' => skip_string(bytes, i, &mut out),
            // Raw strings: r"…", r#"…"#, br#"…"# (the `b` is consumed as a
            // normal byte before we land on the `r`).
            b'r' if !(i > 0 && is_ident_char(bytes[i - 1]) && bytes[i - 1] != b'b') => {
                skip_raw_string(bytes, i, &mut out)
            }
            // Char literal vs lifetime: 'x' / '\n' are literals; 'a is a
            // lifetime (kept — it contains no `crate::`).
            b'\'' => skip_char_literal(bytes, i, &mut out),
            b => {
                out.push(b);
                i + 1
            }
        };
    }
    String::from_utf8(out).expect("stripped source remains valid UTF-8")
}

/// `// …` to the end of the line. The newline itself is left for the caller.
fn skip_line_comment(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// `/* … */`, nested.
fn skip_block_comment(bytes: &[u8], mut i: usize, out: &mut Vec<u8>) -> usize {
    let mut depth = 1usize;
    i += 2;
    while i < bytes.len() && depth > 0 {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
        } else {
            if bytes[i] == b'\n' {
                out.push(b'\n');
            }
            i += 1;
        }
    }
    i
}

/// `"…"`, honouring backslash escapes.
fn skip_string(bytes: &[u8], mut i: usize, out: &mut Vec<u8>) -> usize {
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            b'\n' => {
                out.push(b'\n');
                i += 1;
            }
            _ => i += 1,
        }
    }
    i
}

/// `r"…"` / `r#"…"#`. Not a raw string after all (a bare `r` identifier) → emit
/// the `r` and move on.
fn skip_raw_string(bytes: &[u8], i: usize, out: &mut Vec<u8>) -> usize {
    let mut j = i + 1;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'"') {
        out.push(b'r');
        return i + 1;
    }
    let hashes = j - (i + 1);
    let mut close = vec![b'"'];
    close.extend(std::iter::repeat(b'#').take(hashes));
    let mut at = j + 1;
    while at < bytes.len() && bytes[at..].len() >= close.len() {
        if bytes[at..at + close.len()] == close[..] {
            return at + close.len();
        }
        if bytes[at] == b'\n' {
            out.push(b'\n');
        }
        at += 1;
    }
    at
}

/// `'x'` / `'\n'` are literals and are consumed; `'a` is a lifetime and the
/// quote is kept, since a lifetime contains no `crate::`.
fn skip_char_literal(bytes: &[u8], mut i: usize, out: &mut Vec<u8>) -> usize {
    if bytes.get(i + 1) == Some(&b'\\') {
        i += 3;
        while i < bytes.len() && bytes[i] != b'\'' {
            i += 1;
        }
        return i + 1;
    }
    if bytes.get(i + 2) == Some(&b'\'') && bytes.get(i + 1) != Some(&b'\'') {
        return i + 3;
    }
    out.push(b'\'');
    i + 1
}

/// Byte spans of `use …;` statements in stripped source.
fn use_spans(stripped: &str) -> Vec<(usize, usize)> {
    let bytes = stripped.as_bytes();
    let mut spans = Vec::new();
    let mut search = 0;
    while let Some(found) = stripped[search..].find("use") {
        let start = search + found;
        search = start + 3;
        let before_ok = start == 0 || !is_ident_char(bytes[start - 1]);
        let after_ok = bytes.get(start + 3).is_some_and(u8::is_ascii_whitespace);
        if before_ok && after_ok {
            let end = stripped[start..]
                .find(';')
                .map_or(stripped.len(), |e| start + e + 1);
            spans.push((start, end));
            search = end;
        }
    }
    spans
}

fn read_ident(bytes: &[u8], i: &mut usize) -> String {
    let start = *i;
    while *i < bytes.len() && is_ident_char(bytes[*i]) {
        *i += 1;
    }
    String::from_utf8_lossy(&bytes[start..*i]).into_owned()
}

/// First path segments of a `crate::{…}` brace group (top level only):
/// `crate::{agent::x, ui::y}` yields `agent` and `ui`.
fn brace_group_segments(bytes: &[u8], open: usize) -> Vec<String> {
    let mut segments = Vec::new();
    let mut depth = 0usize;
    let mut expect_segment = false;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                expect_segment = depth == 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                i += 1;
            }
            b',' => {
                expect_segment |= depth == 1;
                i += 1;
            }
            b if depth == 1 && expect_segment && !b.is_ascii_whitespace() => {
                if is_ident_char(b) {
                    segments.push(read_ident(bytes, &mut i));
                } else {
                    i += 1;
                }
                expect_segment = false;
            }
            _ => i += 1,
        }
    }
    segments
}

/// All `crate::<segment>` references in stripped source.
fn crate_refs(stripped: &str) -> Vec<RefSite> {
    const TOKEN: &str = "crate::";
    let bytes = stripped.as_bytes();
    let spans = use_spans(stripped);
    let mut refs = Vec::new();
    let mut search = 0;
    while let Some(found) = stripped[search..].find(TOKEN) {
        let pos = search + found;
        search = pos + TOKEN.len();
        if is_crate_tail(bytes, pos) {
            continue;
        }
        let in_use = spans.iter().any(|&(s, e)| pos >= s && pos < e);
        for segment in refs_after(bytes, pos + TOKEN.len()) {
            refs.push(RefSite {
                offset: pos,
                segment,
                in_use,
            });
        }
    }
    refs
}

/// Whether the `crate::` at `pos` is really the tail of something else —
/// `$crate::` (macros) or a path like `my_crate::`.
fn is_crate_tail(bytes: &[u8], pos: usize) -> bool {
    if pos == 0 {
        return false;
    }
    let prev = bytes[pos - 1];
    is_ident_char(prev) || prev == b':' || prev == b'$'
}

/// The segment(s) a `crate::` names: a brace group's members, or one identifier.
fn refs_after(bytes: &[u8], from: usize) -> Vec<String> {
    let mut i = from;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'{') {
        return brace_group_segments(bytes, i);
    }
    match bytes.get(i).is_some_and(|&b| is_ident_char(b)) {
        true => vec![read_ident(bytes, &mut i)],
        false => Vec::new(),
    }
}

/// Check one module against its allowlist.
fn check_module(rules: &ModuleRules) -> Vec<Violation> {
    let mut violations = Vec::new();
    for file in module_files(rules.name) {
        let content = fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        let stripped = strip_comments_and_strings(&content);
        for site in crate_refs(&stripped) {
            // `self` (`use crate::{self, …}`) names the crate root, which
            // declares only modules — harmless. Own-module refs are fine.
            if site.segment == "self" || site.segment == rules.name {
                continue;
            }
            if rules.allowed.contains(&site.segment.as_str()) {
                continue;
            }
            if !site.in_use && rules.allowed_path_only.contains(&site.segment.as_str()) {
                continue;
            }
            let line_number = stripped[..site.offset].matches('\n').count() + 1;
            let line = content
                .lines()
                .nth(line_number - 1)
                .unwrap_or_default()
                .trim()
                .to_string();
            violations.push(Violation {
                file: file.clone(),
                line_number,
                line,
                segment: site.segment,
                in_use: site.in_use,
            });
        }
    }
    violations
}

fn format_violations(rules: &ModuleRules, violations: &[Violation]) -> String {
    let mut msg = format!(
        "\n{} architecture violation(s) in `{}` (allowed: {:?}; path-only: {:?}):\n",
        violations.len(),
        rules.name,
        rules.allowed,
        rules.allowed_path_only,
    );
    for v in violations {
        let note = if v.in_use && rules.allowed_path_only.contains(&v.segment.as_str()) {
            " (allowed via fully-qualified path only, not `use`)"
        } else {
            ""
        };
        writeln!(
            msg,
            "  {}:{}: references `crate::{}`{note}: {}",
            v.file.display(),
            v.line_number,
            v.segment,
            v.line,
        )
        .unwrap();
    }
    msg.push_str(
        "Fix the import, or — if the architecture is changing on purpose — update \
         MODULE_RULES in tests/architecture_rules.rs plus CLAUDE.md and docs/CONSTITUTION.md.\n",
    );
    msg
}

fn assert_module_clean(name: &str) {
    let rules = MODULE_RULES
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no MODULE_RULES entry for `{name}`"));
    let violations = check_module(rules);
    assert!(
        violations.is_empty(),
        "{}",
        format_violations(rules, &violations)
    );
}

#[test]
fn session_module_purity() {
    assert_module_clean("session");
}

#[test]
fn agent_module_isolation() {
    assert_module_clean("agent");
}

#[test]
fn git_module_independence() {
    assert_module_clean("git");
}

#[test]
fn storage_module_isolation() {
    assert_module_clean("storage");
}

#[test]
fn sync_module_isolation() {
    assert_module_clean("sync");
}

#[test]
fn usage_module_isolation() {
    assert_module_clean("usage");
}

#[test]
fn session_ops_module_isolation() {
    assert_module_clean("session_ops");
}

#[test]
fn cli_module_isolation() {
    assert_module_clean("cli");
}

#[test]
fn util_modules_are_leaves() {
    for name in ["paths", "shell", "workspace"] {
        assert_module_clean(name);
    }
}

#[test]
fn notifications_module_isolation() {
    assert_module_clean("notifications");
}

/// Every module under `src/` must be governed: either a MODULE_RULES entry
/// or an explicit EXEMPT listing. Adding a module without deciding its place
/// in the architecture fails here. Also catches stale rule entries.
#[test]
fn every_module_is_governed() {
    let root = src_root();
    let entries =
        fs::read_dir(&root).unwrap_or_else(|e| panic!("cannot read {}: {e}", root.display()));
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        let name = if path.is_dir() {
            path.file_name()
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            path.file_stem()
        } else {
            continue;
        };
        let name = name
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| panic!("non-UTF-8 path under src/: {}", path.display()))
            .to_string();
        let governed =
            MODULE_RULES.iter().any(|r| r.name == name) || EXEMPT.contains(&name.as_str());
        assert!(
            governed,
            "src/{name} has no architecture rules — add a MODULE_RULES entry \
             (or EXEMPT it) in tests/architecture_rules.rs"
        );
        found.push(name);
    }

    // Stale-entry checks: every rule and allowlist target must still exist.
    for rules in MODULE_RULES {
        assert!(
            found.iter().any(|n| n == rules.name),
            "MODULE_RULES entry `{}` matches nothing under src/ — remove or rename it",
            rules.name
        );
        for target in rules.allowed.iter().chain(rules.allowed_path_only) {
            assert!(
                found.iter().any(|n| n == target),
                "MODULE_RULES entry `{}` allows nonexistent module `{target}`",
                rules.name
            );
        }
    }
}

/// Every persisted proptest seed must still name a source file that exists.
///
/// Proptest's persisted failure-seed store has a layout that is a path contract
/// rather than incidental, and the default `FileFailurePersistence::
/// SourceParallel` writes to *two* places depending on the source file:
///
/// - It climbs the source file's ancestors for a directory holding `lib.rs` or
///   `main.rs`. For anything under `src/` that directory is `src/` itself, so a
///   proptest in `src/agent/backend.rs` persists to its sibling tree,
///   `proptest-regressions/agent/backend.txt`.
/// - An integration test under `tests/` has no such ancestor — there is no
///   `tests/lib.rs`, and none above it — so the climb fails and proptest falls
///   back to `WithSource`, writing a file *beside the source*: a proptest in
///   `tests/render_props.rs` persists to
///   `tests/render_props.proptest-regressions`.
///
/// Proptest resolves that path at run time and **says nothing when it misses** —
/// a renamed or moved source file silently stops re-running its saved cases, so
/// the regression a seed was written for quietly stops being covered. Both
/// locations are swept here, since either kind of seed can be orphaned by a
/// rename.
///
/// This is the check that a wide rename needs and that nothing else provides.
#[test]
fn every_proptest_seed_still_names_a_live_source_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut orphans = Vec::new();

    // SourceParallel: the tree is rooted at `src/`, so the seed's relative path
    // names exactly one source file. `tests/` is deliberately not a candidate —
    // seeds for an integration test never land here, and accepting one would
    // let an unrelated `tests/foo.rs` mask a genuinely orphaned seed.
    let seeds_dir = root.join("proptest-regressions");
    if seeds_dir.exists() {
        for seed in collect_files_with_extension(&seeds_dir, "txt") {
            let rel = seed
                .strip_prefix(&seeds_dir)
                .expect("seed is under proptest-regressions/")
                .with_extension("rs");
            if !root.join("src").join(&rel).exists() {
                orphans.push(format!(
                    "  proptest-regressions/{} -> src/{} does not exist",
                    rel.with_extension("txt").display(),
                    rel.display()
                ));
            }
        }
    }

    // WithSource fallback: a seed beside its integration test.
    let tests_dir = root.join("tests");
    for seed in collect_files_with_extension(&tests_dir, "proptest-regressions") {
        let source = seed.with_extension("rs");
        if !source.exists() {
            let rel = |p: &Path| {
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .display()
                    .to_string()
                    .replace('\\', "/")
            };
            orphans.push(format!(
                "  {} -> {} does not exist",
                rel(&seed),
                rel(&source)
            ));
        }
    }

    assert!(
        orphans.is_empty(),
        "orphaned proptest seeds — the source file moved or was renamed without \
         its seed, so proptest silently stopped replaying these cases:\n{}\n\
         Move the seed alongside the source file, or delete it if the property \
         is gone.",
        orphans.join("\n")
    );
}

/// Every file under `dir` (recursively) with the given extension.
fn collect_files_with_extension(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => panic!("cannot read {}: {e}", dir.display()),
    };
    for entry in entries {
        let path = entry.expect("readable directory entry").path();
        if path.is_dir() {
            out.extend(collect_files_with_extension(&path, ext));
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
    out.sort();
    out
}
