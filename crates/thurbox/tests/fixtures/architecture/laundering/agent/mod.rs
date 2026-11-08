pub mod tmux;

pub use tmux::Index as Panes;

// A re-export through an imported name, not a child path.
use crate::agent::tmux as adapter;
pub use adapter::spawn as start;
