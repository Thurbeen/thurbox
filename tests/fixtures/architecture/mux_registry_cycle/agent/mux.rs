use super::control_mode::ControlMode;
use super::registry::{LocalMuxTransport, DEFAULT_MUX};

pub struct MuxBackend {
    pub control: ControlMode,
    pub transport: LocalMuxTransport,
}
