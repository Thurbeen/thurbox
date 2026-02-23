pub mod backend;
pub mod claude;
pub mod control_mode;
pub mod input;
pub mod provider;
pub mod registry;
pub mod tmux;
pub mod vm;

pub use backend::{Session, SessionBackend};
pub use provider::AgentProvider;
pub use registry::BackendRegistry;
pub use vm::{host_to_vm_path, QemuVmBackend, VmManager};
