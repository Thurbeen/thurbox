use crate::agent::{
    control_mode::{self as cm, ControlMode},
    tmux as this,
};

pub enum WindowRole {
    Agent,
}

pub struct TmuxBackend(ControlMode);

// Through the parent's re-export: the trait lives in `backend`.
impl crate::agent::SessionBackend for TmuxBackend {
    fn snapshot(&self) -> cm::PaneSnapshot {
        cm::PaneSnapshot
    }
    fn stamp(&self, _role: this::WindowRole) {}
}
