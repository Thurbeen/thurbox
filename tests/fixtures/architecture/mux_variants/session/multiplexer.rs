pub enum Multiplexer {
    Tmux,
    Psmux,
    Rmux,
    Herdr,
}

impl Multiplexer {
    pub const ALL: [Multiplexer; 4] = [Self::Tmux, Self::Psmux, Self::Rmux, Self::Herdr];
}
