//! The tmux command and control-mode protocol, which tmux and psmux both
//! speak: the helper their adapters share. It knows the backend contract and
//! never an adapter: a way one multiplexer differs from the other is a body in
//! that multiplexer's adapter (`TmuxCompatible`), never a branch here.

pub mod control_mode;
pub mod server;
pub mod socket;
pub mod transport;
