use super::tmux::WindowRole;

pub struct PaneSize;

pub trait SessionBackend {
    fn snapshot(&self) -> crate::agent::control_mode::PaneSnapshot;
    fn stamp(&self, role: WindowRole);
}
