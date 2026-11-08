use runtime_mux::tmux as t;
use runtime_mux as rm;

pub fn spawn() {
    t::spawn();
    rm::wiring::local_only();
    runtime_mux::psmux::spawn();
    // runtime_mux::rmux::spawn() in a comment is no reference.
    let _ = "runtime_mux::rmux";
    crate::backend::for_hosts();
}
