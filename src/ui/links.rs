//! Link detection for the mouse layer.
//!
//! The logic moved to [`crate::session::links`] so the v2 kernel can use it too
//! — it cannot import `ui`. This re-export keeps every v1 call site unchanged.

pub use crate::session::links::{detect_urls, extract_screen_rows, url_at_position, DetectedLink};
