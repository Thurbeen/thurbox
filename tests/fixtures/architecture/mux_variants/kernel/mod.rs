use crate::session::Multiplexer;
fn by_path() -> bool {
    matches!(Multiplexer::ALL[0], crate::session::Multiplexer::Psmux)
}
fn by_module() -> crate::session::multiplexer::Multiplexer { crate::session::multiplexer::Multiplexer::Rmux }
fn free() -> usize { Multiplexer::ALL.len() }
fn imported() -> Multiplexer {
    Multiplexer::Herdr
}
use crate::session::Multiplexer::Tmux;
#[cfg(test)]
mod tests {
    fn pins_rmux() -> crate::session::Multiplexer { crate::session::Multiplexer::Rmux }
}
