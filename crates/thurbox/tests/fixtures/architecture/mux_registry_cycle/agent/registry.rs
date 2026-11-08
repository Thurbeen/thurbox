use crate::agent::{mux, psmux::PsmuxBackend, tmux::TmuxBackend};

pub struct LocalMuxTransport;
pub const DEFAULT_MUX: &str = "tmux";

pub fn configured() -> (TmuxBackend, PsmuxBackend, mux::MuxBackend) {
    unimplemented!()
}
