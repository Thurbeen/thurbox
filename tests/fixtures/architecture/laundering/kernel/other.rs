fn f(
    _a: crate::kernel::Index,
    _b: crate::agent::Panes,
) { use crate::agent::{tmux::{self}};
    // `tmux` is bound by the import above.
    tmux::spawn();
}
