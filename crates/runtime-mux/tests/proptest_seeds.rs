//! Persisted proptest seeds still name live source files.

use std::fs;
use std::path::Path;

/// Every persisted proptest seed still names a source file: a moved source
/// silently stops replaying its saved cases.
#[test]
fn every_proptest_seed_still_names_a_live_source_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let seeds = root.join("proptest-regressions");
    let mut orphans = Vec::new();
    let mut stack = vec![seeds.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("readable entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "txt") {
                let rel = path.strip_prefix(&seeds).unwrap().with_extension("rs");
                if !root.join("src").join(&rel).exists() {
                    orphans.push(rel.display().to_string());
                }
            }
        }
    }
    assert!(
        orphans.is_empty(),
        "orphaned proptest seeds for src/{orphans:?}"
    );
}
