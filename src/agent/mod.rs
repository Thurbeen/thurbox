pub mod backend;
pub mod claude;
pub mod control_mode;
pub mod devcontainer;
pub mod input;
pub mod provider;
pub mod registry;
pub mod tmux;
pub mod vm;

pub use backend::{Session, SessionBackend};
pub use devcontainer::{
    detect_runtime, host_to_container_path, ContainerManager, ContainerRuntime,
    DevcontainerBackend, DEFAULT_ALLOWLIST, DEFAULT_CONTAINERFILE, INIT_FIREWALL_SH,
};
pub use provider::AgentProvider;
pub use registry::BackendRegistry;
pub use vm::{host_to_vm_path, QemuVmBackend, VmManager};
