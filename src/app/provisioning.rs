//! VM and container provisioning for Thurbox sessions.

use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use tracing::{error, warn};

use crate::session::{SessionInfo, SessionStatus};

use super::{helpers, App};

impl App {
    /// Start asynchronous VM provisioning for a sandbox session.
    ///
    /// Called when the user selects "VM" from the session mode modal.
    /// The VM boots in the background; `handle_vm_ready` spawns the actual session.
    pub(crate) fn start_vm_provisioning(&mut self) {
        if self.vm_provisioning {
            self.set_info("VM is already being provisioned...".to_string());
            return;
        }

        if self.backends.get("qemu-vm").is_none() {
            self.set_error("QEMU VM backend not available".to_string());
            return;
        }

        let manager = match self.vm_manager {
            Some(ref m) => Arc::clone(m),
            None => {
                self.set_error("VM manager not initialized".to_string());
                return;
            }
        };

        let vm_id = uuid::Uuid::new_v4().to_string();
        self.vm_provisioning = true;
        self.vm_provisioning_id = Some(vm_id.clone());

        // Create a placeholder session visible in the session list during provisioning.
        let name = self.next_session_name();
        let mut placeholder = SessionInfo::new(name);
        placeholder.status = SessionStatus::Provisioning;
        placeholder.vm_id = Some(vm_id.clone());
        placeholder.provisioning_step = Some("Checking tools...".to_string());

        self.vm_placeholder = Some(placeholder);

        self.set_info("Provisioning VM...".to_string());

        // Collect repo paths for rsync into the VM.
        // pending_repo_path and pending_all_repos are already set by spawn_session().
        let repo_paths: Vec<PathBuf> = if let Some(ref all_repos) = self.pending_all_repos {
            all_repos.clone()
        } else if let Some(ref path) = self.pending_repo_path {
            vec![path.clone()]
        } else {
            Vec::new()
        };

        let config = crate::session::VmConfig::default();

        // Record in DB
        if let Err(e) = self.db.insert_vm(
            &vm_id,
            None,
            &crate::session::VmState::Provisioning,
            0, // SSH port allocated later by VmManager
            &config,
        ) {
            error!("Failed to insert VM record: {e}");
            self.set_error(format!("Failed to create VM: {e}"));
            self.vm_provisioning = false;
            self.vm_provisioning_id = None;
            return;
        }

        // Spawn background thread for VM provisioning (QEMU + cloud-init + SSH + rsync).
        let (tx, rx) = mpsc::channel();
        let (step_tx, step_rx) = mpsc::channel();
        self.vm_provision_rx = Some(rx);
        self.vm_provision_step_rx = Some(step_rx);
        self.vm_provisioning_step = "Checking tools...".to_string();
        let vm_id_thread = vm_id.clone();
        let config_thread = config;
        tracing::info!(vm_id = %vm_id, "VM provisioning started");

        let spawn_result = std::thread::Builder::new()
            .name(format!("vm-provision-{}", &vm_id[..8]))
            .spawn(move || {
                let mgr = manager.lock().unwrap();
                let result = mgr.create_vm(&vm_id_thread, &config_thread, &repo_paths, &step_tx);
                drop(mgr);
                match result {
                    Ok(()) => {
                        let _ = tx.send(Ok(vm_id_thread));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("{e:#}")));
                    }
                }
            });

        if let Err(e) = spawn_result {
            error!("Failed to spawn VM provisioning thread: {e}");
            self.set_error(format!("Failed to start VM provisioning: {e}"));
            self.vm_provisioning = false;
            self.vm_provisioning_id = None;
            self.vm_provision_rx = None;
        }
    }

    /// Poll for VM provisioning results and step updates from the background thread.
    pub(crate) fn poll_vm_provision(&mut self) {
        // Drain step updates (keep the latest).
        if let Some(ref rx) = self.vm_provision_step_rx {
            while let Ok(step) = rx.try_recv() {
                self.vm_provisioning_step = step.clone();
                if let Some(ref mut ph) = self.vm_placeholder {
                    ph.provisioning_step = Some(step);
                }
            }
        }

        let result = match self.vm_provision_rx {
            Some(ref rx) => match rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                    "VM provisioning thread terminated unexpectedly".to_string(),
                )),
            },
            None => None,
        };

        if let Some(result) = result {
            self.vm_provision_rx = None;
            self.vm_provision_step_rx = None;
            self.vm_provisioning_step.clear();
            match result {
                Ok(vm_id) => self.handle_vm_ready(&vm_id),
                Err(error) => self.handle_vm_failed(&error),
            }
        }
    }

    /// Handle a successfully provisioned VM — route through role selection before
    /// spawning. The actual spawn happens in `do_spawn_session` which checks
    /// `pending_vm_id` to use the VM backend.
    pub(super) fn handle_vm_ready(&mut self, vm_id: &str) {
        self.vm_provisioning = false;
        self.vm_provisioning_id = None;
        self.set_info("VM ready — starting session...".to_string());

        let backend = match self.backends.get("qemu-vm") {
            Some(b) => Arc::clone(b),
            None => {
                self.set_error("QEMU VM backend disappeared".to_string());
                return;
            }
        };

        // Establish SSH control mode connection for the newly provisioned VM.
        if let Err(e) = backend.prepare_vm(vm_id) {
            error!("Failed to establish VM control mode: {e}");
            self.set_error(format!("VM ready but control mode failed: {e:#}"));
            return;
        }

        // Take the pending config built during session mode selection, remap host
        // paths to VM paths.
        let mut config = self.pending_vm_config.take().unwrap_or_default();
        if let Some(ref cwd) = config.cwd {
            config.cwd = Some(crate::agent::host_to_vm_path(cwd));
        }
        config.additional_dirs = config
            .additional_dirs
            .iter()
            .map(|p| crate::agent::host_to_vm_path(p))
            .collect();
        self.pending_repo_path = None;
        self.pending_all_repos = None;
        self.pending_vm_repo_paths = None;

        // Write project MCP servers as .mcp.json into the VM working directory.
        if let Some(mcp_servers) = self.pending_vm_mcp_servers.take() {
            if let Some(ref cwd) = config.cwd {
                if let Some(ref mgr) = self.vm_manager {
                    let mgr = mgr.lock().unwrap();
                    if let Some(instance) = mgr.get_instance(vm_id) {
                        if let Err(e) = helpers::write_vm_mcp_json(&instance, cwd, &mcp_servers) {
                            warn!("Failed to write .mcp.json into VM: {e:#}");
                        }
                    }
                }
            }
        }

        // Store VM ID so `do_spawn_session` uses the VM backend.
        self.pending_vm_id = Some(vm_id.to_string());

        // Route through role selection (prepare_spawn handles 0/1/2+ roles).
        self.prepare_spawn(config, Vec::new());
    }

    /// Handle a failed VM provisioning attempt.
    pub(super) fn handle_vm_failed(&mut self, error: &str) {
        self.vm_provisioning = false;
        let vm_id = self.vm_provisioning_id.take();
        self.set_error(format!("VM provisioning failed: {error}"));
        self.pending_repo_path = None;
        self.pending_all_repos = None;
        self.pending_vm_repo_paths = None;
        self.pending_vm_config = None;
        self.pending_vm_id = None;
        self.pending_vm_mcp_servers = None;

        // Update placeholder to show error state (keep it visible).
        if let Some(ref mut ph) = self.vm_placeholder {
            ph.status = SessionStatus::Error;
            ph.provisioning_step = Some(error.to_string());
        }

        // Update DB record
        if let Some(id) = &vm_id {
            let _ = self.db.update_vm_state(
                id,
                &crate::session::VmState::Failed(error.to_string()),
                None,
                Some(error),
            );
        }
    }

    /// Start container provisioning in a background thread.
    pub(crate) fn start_container_provisioning(&mut self) {
        if self.container_provisioning {
            self.set_info("Container is already being provisioned...".to_string());
            return;
        }

        if self.backends.get("devcontainer").is_none() {
            self.set_error("Container backend not available".to_string());
            return;
        }

        let manager = match self.container_manager {
            Some(ref m) => Arc::clone(m),
            None => {
                self.set_error("Container manager not initialized".to_string());
                return;
            }
        };

        let container_id = uuid::Uuid::new_v4().to_string();
        self.container_provisioning = true;
        self.container_provisioning_id = Some(container_id.clone());

        // Create a placeholder session visible in the session list during provisioning.
        let name = self.next_session_name();
        let mut placeholder = SessionInfo::new(name);
        placeholder.status = SessionStatus::Provisioning;
        placeholder.container_id = Some(container_id.clone());
        placeholder.provisioning_step = Some("Preparing workspace...".to_string());

        self.container_placeholder = Some(placeholder);

        self.set_info("Provisioning container...".to_string());

        // Determine repo path for the container workspace.
        let repo_path: Option<PathBuf> = if let Some(ref all_repos) = self.pending_all_repos {
            Some(all_repos[0].clone())
        } else {
            self.pending_repo_path.clone()
        };

        let containerfile_name = self
            .pending_containerfile_name
            .take()
            .unwrap_or_else(|| "default".to_string());
        let config = crate::session::ContainerConfig {
            containerfile: Some(containerfile_name.clone()),
            ..crate::session::ContainerConfig::default()
        };

        // Record in DB
        if let Err(e) = self.db.insert_container(
            &container_id,
            None,
            &crate::session::ContainerState::Building,
            &config,
        ) {
            error!("Failed to insert container record: {e}");
            self.set_error(format!("Failed to create container: {e}"));
            self.container_provisioning = false;
            self.container_provisioning_id = None;
            return;
        }

        // Spawn background thread for container provisioning.
        let (tx, rx) = mpsc::channel();
        let (step_tx, step_rx) = mpsc::channel();
        self.container_provision_rx = Some(rx);
        self.container_provision_step_rx = Some(step_rx);
        self.container_provisioning_step = "Preparing workspace...".to_string();
        let container_id_thread = container_id.clone();
        let config_thread = config;
        let containerfile_thread = containerfile_name;
        tracing::info!(container_id = %container_id, "Container provisioning started");

        let spawn_result = std::thread::Builder::new()
            .name(format!("dc-provision-{}", &container_id[..8]))
            .spawn(move || {
                let mgr = manager.lock().unwrap();
                let result = mgr.create_container(
                    &container_id_thread,
                    &config_thread,
                    &containerfile_thread,
                    repo_path.as_deref(),
                    &step_tx,
                );
                drop(mgr);
                match result {
                    Ok(()) => {
                        let _ = tx.send(Ok(container_id_thread));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("{e:#}")));
                    }
                }
            });

        if let Err(e) = spawn_result {
            error!("Failed to spawn container provisioning thread: {e}");
            self.set_error(format!("Failed to start container provisioning: {e}"));
            self.container_provisioning = false;
            self.container_provisioning_id = None;
            self.container_provision_rx = None;
        }
    }

    /// Poll for container provisioning results from the background thread.
    pub(crate) fn poll_container_provision(&mut self) {
        // Drain step updates (keep the latest).
        if let Some(ref rx) = self.container_provision_step_rx {
            while let Ok(step) = rx.try_recv() {
                self.container_provisioning_step = step.clone();
                if let Some(ref mut ph) = self.container_placeholder {
                    ph.provisioning_step = Some(step);
                }
            }
        }

        let result = match self.container_provision_rx {
            Some(ref rx) => match rx.try_recv() {
                Ok(r) => Some(r),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                    "Container provisioning thread terminated unexpectedly".to_string(),
                )),
            },
            None => None,
        };

        if let Some(result) = result {
            self.container_provision_rx = None;
            self.container_provision_step_rx = None;
            self.container_provisioning_step.clear();
            match result {
                Ok(container_id) => self.handle_container_ready(&container_id),
                Err(error) => self.handle_container_failed(&error),
            }
        }
    }

    /// Handle successful container provisioning.
    fn handle_container_ready(&mut self, container_id: &str) {
        self.container_provisioning = false;
        self.container_provisioning_id = None;
        self.set_info("Container ready — starting session...".to_string());

        let backend = match self.backends.get("devcontainer") {
            Some(b) => Arc::clone(b),
            None => {
                self.set_error("Container backend disappeared".to_string());
                return;
            }
        };

        // Establish docker exec control mode connection.
        if let Err(e) = backend.prepare_vm(container_id) {
            error!("Failed to establish container control mode: {e}");
            self.set_error(format!("Container ready but control mode failed: {e:#}"));
            return;
        }

        // Take the pending config, remap host paths to container paths.
        let mut config = self.pending_container_config.take().unwrap_or_default();
        if let Some(ref cwd) = config.cwd {
            config.cwd = Some(crate::agent::host_to_container_path(cwd));
        }
        config.additional_dirs = config
            .additional_dirs
            .iter()
            .map(|p| crate::agent::host_to_container_path(p))
            .collect();
        self.pending_repo_path = None;
        self.pending_all_repos = None;

        // Write project MCP servers as .mcp.json into the container working directory.
        if let Some(mcp_servers) = self.pending_container_mcp_servers.take() {
            if let Some(ref cwd) = config.cwd {
                if let Some(ref mgr) = self.container_manager {
                    if let Ok(mgr) = mgr.lock() {
                        let rt = mgr.runtime().cmd();
                        if let Some(instance) = mgr.get_instance(container_id) {
                            if let Some(ref docker_id) = instance.docker_container_id {
                                if let Err(e) = helpers::write_container_mcp_json(
                                    rt,
                                    docker_id,
                                    cwd,
                                    &mcp_servers,
                                ) {
                                    warn!("Failed to write .mcp.json into container: {e:#}");
                                }
                            }
                        }
                    }
                }
            }
        }

        // Store container ID so `do_spawn_session` uses the devcontainer backend.
        self.pending_container_id = Some(container_id.to_string());

        // Route through role selection.
        self.prepare_spawn(config, Vec::new());
    }

    /// Handle a failed container provisioning attempt.
    fn handle_container_failed(&mut self, error: &str) {
        self.container_provisioning = false;
        let container_id = self.container_provisioning_id.take();
        self.set_error(format!("Container provisioning failed: {error}"));
        self.pending_repo_path = None;
        self.pending_all_repos = None;
        self.pending_container_config = None;
        self.pending_container_id = None;
        self.pending_container_mcp_servers = None;

        // Update placeholder to show error state.
        if let Some(ref mut ph) = self.container_placeholder {
            ph.status = SessionStatus::Error;
            ph.provisioning_step = Some(error.to_string());
        }

        // Update DB record
        if let Some(id) = &container_id {
            let _ = self.db.update_container_state(
                id,
                &crate::session::ContainerState::Failed(error.to_string()),
                None,
                Some(error),
            );
        }
    }
}
