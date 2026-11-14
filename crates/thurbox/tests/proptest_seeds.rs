//! Persisted proptest seeds still name live source files.

use std::fs;
use std::path::{Path, PathBuf};

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
