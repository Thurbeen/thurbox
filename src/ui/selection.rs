//! Mouse text selection.
//!
//! The logic moved to [`crate::session::selection`] so the v2 kernel can use it
//! too — it cannot import `ui`. This re-export keeps every v1 call site
//! unchanged.

pub use crate::session::selection::{
    extract_text_from_buffer, extract_text_from_screen, highlight_buffer, PaneBounds, Selection,
    TermPos,
};
