//! Repository-level checks for files that should not ship with thurbox.

use std::path::Path;

#[test]
fn one_off_review_artifacts_are_not_committed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let forbidden = [
        "media/focus.gif",
        "media/focus",
        "media/interface-tab-after.gif",
        "media/interface-tab-before.gif",
        "media/performance-page-after.webp",
        "media/performance-page-before.webp",
        "media/performance-page-mobile.webp",
        "media/plugins-page",
        "media/reviews",
        "media/search-full-content.gif",
        "media/session-groups.gif",
        "website/assets/ui-review",
        "website/css/ui-review.css",
        "website/docs/ui-review.html",
    ];

    let present: Vec<_> = forbidden
        .iter()
        .filter(|path| root.join(path).exists())
        .copied()
        .collect();
    assert!(
        present.is_empty(),
        "one-off review artifacts must stay out of the repository: {present:?}"
    );
}
