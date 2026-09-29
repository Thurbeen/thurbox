//! The tmux command and control-mode protocol, as a helper an adapter may use.
//! Grammar only: it knows the backend contract and never the adapter using it.

pub mod control_mode;
pub mod transport;
