//! QEMU/KVM virtual machine lifecycle management.
//!
//! Manages the full VM lifecycle: create, start, stop, destroy. Each VM runs
//! a QEMU process with a CoW overlay disk, cloud-init for provisioning, and
//! SSH for connectivity. VMs provide isolated sandboxed environments for
//! Claude Code sessions.
//!
//! ## VM storage layout
//!
//! ```text
//! ~/.local/share/thurbox/vms/<vm-uuid>/
//!   disk.qcow2        — CoW overlay (base image unchanged)
//!   cloud-init.iso    — cloud-init nocloud ISO
//!   qemu.pid          — QEMU process PID
//!   ssh_key           — ephemeral SSH private key
//!   ssh_key.pub       — ephemeral SSH public key
//!   ssh-control       — SSH ControlMaster socket
//!
//! ~/.local/share/thurbox/images/
//!   debian-13-genericcloud-amd64.qcow2  — base image (shared)
//! ```

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

use crate::agent::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};
use crate::agent::control_mode::{
    self, CommandResponse, ControlModeReader, ControlModeWriter, Notification,
    PaneSendersMapShared, PANE_CHANNEL_CAPACITY,
};
use crate::session::{VmConfig, VmState};

/// A running or stopped VM instance.
pub struct VmInstance {
    /// Unique VM identifier.
    pub id: String,
    /// Current state.
    pub state: VmState,
    /// SSH port on localhost (host-forwarded to guest port 22).
    pub ssh_port: u16,
    /// Configuration used to create this VM.
    pub config: VmConfig,
    /// Directory containing VM files (disk, cloud-init, keys).
    pub vm_dir: PathBuf,
    /// QEMU child process handle (None if not running).
    process: Option<Child>,
}

impl VmInstance {
    /// Path to the CoW overlay disk.
    pub fn disk_path(&self) -> PathBuf {
        self.vm_dir.join("disk.qcow2")
    }

    /// Path to the cloud-init ISO.
    pub fn cloud_init_path(&self) -> PathBuf {
        self.vm_dir.join("cloud-init.iso")
    }

    /// Path to the ephemeral SSH private key.
    pub fn ssh_key_path(&self) -> PathBuf {
        self.vm_dir.join("ssh_key")
    }

    /// Path to the SSH ControlMaster socket.
    ///
    /// Uses `/tmp/` instead of the VM directory because Unix domain socket paths
    /// are limited to 108 bytes, and SSH appends a random suffix (~16 chars).
    /// The VM dir path with a UUID easily exceeds this limit.
    pub fn ssh_control_path(&self) -> PathBuf {
        let short_id = if self.id.len() > 8 {
            &self.id[..8]
        } else {
            &self.id
        };
        PathBuf::from(format!("/tmp/thurbox-ssh-{short_id}"))
    }

    /// SSH user for the VM.
    pub fn ssh_user(&self) -> &str {
        "thurbox"
    }
}

/// Manages QEMU VM lifecycle: create, start, stop, destroy.
///
/// Handles port allocation, image management, and cloud-init provisioning.
pub struct VmManager {
    /// Base directory for VM storage (~/.local/share/thurbox/vms/).
    vms_dir: PathBuf,
    /// Base directory for shared images (~/.local/share/thurbox/images/).
    images_dir: PathBuf,
    /// Active VMs keyed by VM ID.
    instances: Mutex<HashMap<String, VmInstance>>,
    /// Port allocator state — next port to try.
    next_port: Mutex<u16>,
}

/// Base path for synced repositories inside the VM.
const VM_REPOS_DIR: &str = "/home/thurbox/repos";

/// Map a host-side path to the corresponding VM-side path.
///
/// Uses the final path component as the directory name under `/home/thurbox/repos/`.
/// For example: `/home/user/projects/my-app` → `/home/thurbox/repos/my-app`
pub fn host_to_vm_path(host_path: &Path) -> PathBuf {
    let basename = host_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("repo"));
    PathBuf::from(VM_REPOS_DIR).join(basename)
}

/// SSH connection timeout when polling for VM readiness.
const SSH_CONNECT_TIMEOUT_SECS: u32 = 2;

/// Maximum time to wait for a VM to become SSH-ready.
const VM_BOOT_TIMEOUT: Duration = Duration::from_secs(120);

/// SSH poll interval during boot.
const SSH_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Starting port for VM SSH forwarding.
const BASE_SSH_PORT: u16 = 22200;

/// Build the download URL for a Debian cloud image.
fn debian_image_url(image_name: &str) -> String {
    format!("https://cloud.debian.org/images/cloud/trixie/latest/{image_name}")
}

impl VmManager {
    /// Create a new VM manager.
    ///
    /// `data_dir` is typically `~/.local/share/thurbox/`.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            vms_dir: data_dir.join("vms"),
            images_dir: data_dir.join("images"),
            instances: Mutex::new(HashMap::new()),
            next_port: Mutex::new(BASE_SSH_PORT),
        }
    }

    /// Check if QEMU and KVM are available (enough to show the "Sandbox VM" option).
    ///
    /// This only checks core requirements. Provisioning tools (genisoimage, ssh-keygen)
    /// are checked later at VM creation time via `check_provisioning_tools`.
    pub fn check_available() -> Result<()> {
        let output = Command::new("qemu-system-x86_64")
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("qemu-system-x86_64 is not installed or not in PATH")?;

        if !output.status.success() {
            bail!("qemu-system-x86_64 --version failed");
        }

        // Check /dev/kvm access for hardware acceleration.
        let kvm_path = Path::new("/dev/kvm");
        if !kvm_path.exists() {
            bail!("/dev/kvm not found — KVM is required for VM sessions");
        }

        debug!("QEMU + KVM available");
        Ok(())
    }

    /// Check that all provisioning tools are installed.
    ///
    /// Called at VM creation time, not at startup. Missing tools produce a clear
    /// error message telling the user what to install.
    pub fn check_provisioning_tools() -> Result<()> {
        // Check for cloud-init ISO creation tool.
        let has_genisoimage = Command::new("genisoimage")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();

        let has_mkisofs = Command::new("mkisofs")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();

        if !has_genisoimage && !has_mkisofs {
            bail!("Neither genisoimage nor mkisofs found — needed for cloud-init ISO creation. Install cdrtools or cdrkit.");
        }

        // Check for ssh-keygen.
        let has_ssh_keygen = Command::new("ssh-keygen")
            .arg("-V")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();

        if !has_ssh_keygen {
            bail!("ssh-keygen not found — needed for VM SSH key generation");
        }

        // Check for rsync (needed to sync host repos into the VM).
        let has_rsync = Command::new("rsync")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();

        if !has_rsync {
            bail!("rsync not found — needed to sync repositories into the VM");
        }

        // Check for curl (needed to download base images).
        let has_curl = Command::new("curl")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();

        if !has_curl {
            bail!("curl not found — needed to download VM base images");
        }

        debug!("All VM provisioning tools available");
        Ok(())
    }

    /// Ensure the VM and images directories exist.
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.vms_dir).context("Failed to create VMs directory")?;
        std::fs::create_dir_all(&self.images_dir).context("Failed to create images directory")?;
        Ok(())
    }

    /// Download the base image if it doesn't already exist.
    ///
    /// Downloads to a `.partial` temporary file and atomically renames on success,
    /// so interrupted downloads don't leave a corrupt image behind.
    pub fn ensure_base_image(
        &self,
        image_name: &str,
        progress: &std::sync::mpsc::Sender<String>,
    ) -> Result<()> {
        let image_path = self.images_dir.join(image_name);
        if image_path.exists() {
            debug!(image = %image_name, "Base image already cached");
            return Ok(());
        }

        self.ensure_dirs()?;

        let url = debian_image_url(image_name);
        let partial_path = self.images_dir.join(format!("{image_name}.partial"));

        Self::report_step(progress, &format!("Downloading {image_name}..."));
        info!(url = %url, dest = %image_path.display(), "Downloading base image");

        let output = Command::new("curl")
            .args(["-fsSL", "-o", &partial_path.to_string_lossy(), &url])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to run curl for base image download")?;

        if !output.status.success() {
            let _ = std::fs::remove_file(&partial_path);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "Failed to download base image from {url}: {}",
                stderr.trim()
            );
        }

        std::fs::rename(&partial_path, &image_path)
            .context("Failed to rename downloaded image from .partial to final path")?;

        info!(image = %image_name, "Base image downloaded successfully");
        Ok(())
    }

    /// Check if the base image exists.
    pub fn has_base_image(&self, image_name: &str) -> bool {
        self.images_dir.join(image_name).exists()
    }

    /// Return the path to a base image.
    pub fn base_image_path(&self, image_name: &str) -> PathBuf {
        self.images_dir.join(image_name)
    }

    /// Allocate the next available SSH port.
    ///
    /// Probes ports starting from the current counter, skipping any that are
    /// already bound (e.g., by orphaned QEMU processes from a previous run).
    fn allocate_port(&self) -> u16 {
        let mut port = self.next_port.lock().unwrap();
        loop {
            let candidate = *port;
            *port += 1;

            // Try to bind the port to check availability.
            match std::net::TcpListener::bind(("127.0.0.1", candidate)) {
                Ok(_listener) => {
                    // Port is free — the listener is dropped, releasing it for QEMU.
                    return candidate;
                }
                Err(_) => {
                    debug!(port = candidate, "Port in use, skipping");
                    // Safety valve: don't scan forever.
                    if candidate > BASE_SSH_PORT + 100 {
                        warn!("Exhausted port range, using {}", candidate + 1);
                        *port += 1;
                        return candidate + 1;
                    }
                }
            }
        }
    }

    /// Sync host repositories into the VM via rsync.
    ///
    /// Creates `/home/thurbox/repos/` in the VM and rsyncs each host repo into it.
    /// Must be called after SSH is ready.
    fn sync_repos(&self, instance: &VmInstance, repo_paths: &[PathBuf]) -> Result<()> {
        if repo_paths.is_empty() {
            return Ok(());
        }

        let key_path = instance.ssh_key_path();
        let ssh_port = instance.ssh_port.to_string();
        let user = instance.ssh_user();

        // Create the repos directory in the VM.
        let mkdir_status = Command::new("ssh")
            .args([
                "-i",
                &key_path.to_string_lossy(),
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "BatchMode=yes",
                "-p",
                &ssh_port,
                &format!("{user}@localhost"),
                "mkdir",
                "-p",
                VM_REPOS_DIR,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .context("Failed to create repos directory in VM")?;

        if !mkdir_status.success() {
            bail!("Failed to create {VM_REPOS_DIR} in VM");
        }

        let ssh_cmd = format!(
            "ssh -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o BatchMode=yes -p {}",
            key_path.display(),
            ssh_port,
        );

        for repo in repo_paths {
            let basename = repo
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("repo"));
            let vm_dest = format!(
                "{user}@localhost:{VM_REPOS_DIR}/{}/",
                basename.to_string_lossy()
            );

            // Trailing `/` on source copies contents, not the directory itself.
            let mut source = repo.to_string_lossy().to_string();
            if !source.ends_with('/') {
                source.push('/');
            }

            info!(
                vm_id = %instance.id,
                repo = %repo.display(),
                "Syncing repository into VM"
            );

            // Files are owned by `thurbox` automatically since rsync runs as that user.
            let output = Command::new("rsync")
                .args(["-az", "--delete", "-e", &ssh_cmd, &source, &vm_dest])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .context(format!("Failed to rsync {}", repo.display()))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("rsync failed for {}: {}", repo.display(), stderr.trim());
            }

            debug!(
                vm_id = %instance.id,
                repo = %repo.display(),
                "Repository synced to VM"
            );
        }

        info!(
            vm_id = %instance.id,
            count = repo_paths.len(),
            "All repositories synced into VM"
        );
        Ok(())
    }

    /// Send a progress update to the TUI (best-effort, never fails).
    fn report_step(progress: &std::sync::mpsc::Sender<String>, msg: &str) {
        let _ = progress.send(msg.to_string());
    }

    /// Create and start a new VM.
    ///
    /// This is an async-friendly method that performs the full provisioning:
    /// 1. Generate SSH keypair
    /// 2. Create CoW overlay disk
    /// 3. Generate cloud-init ISO
    /// 4. Start QEMU process
    /// 5. Wait for SSH readiness
    /// 6. Wait for cloud-init
    /// 7. Sync host repositories into the VM
    ///
    /// The `progress` sender delivers step descriptions to the TUI status bar.
    pub fn create_vm(
        &self,
        vm_id: &str,
        config: &VmConfig,
        repo_paths: &[PathBuf],
        progress: &std::sync::mpsc::Sender<String>,
    ) -> Result<()> {
        Self::report_step(progress, "Checking provisioning tools...");
        Self::check_provisioning_tools()?;

        self.ensure_base_image(&config.base_image, progress)?;

        let vm_dir = self.vms_dir.join(vm_id);
        std::fs::create_dir_all(&vm_dir).context("Failed to create VM directory")?;

        let ssh_port = self.allocate_port();

        let mut instance = VmInstance {
            id: vm_id.to_string(),
            state: VmState::Provisioning,
            ssh_port,
            config: config.clone(),
            vm_dir: vm_dir.clone(),
            process: None,
        };

        info!(vm_id = %vm_id, ssh_port = ssh_port, "Provisioning VM");

        // Step 1: Generate ephemeral SSH keypair.
        Self::report_step(progress, "Generating SSH keypair...");
        self.generate_ssh_keypair(&instance)?;

        // Step 2: Create CoW overlay disk.
        Self::report_step(progress, "Creating disk overlay...");
        self.create_overlay_disk(&instance)?;

        // Step 3: Generate cloud-init ISO.
        Self::report_step(progress, "Building cloud-init ISO...");
        self.create_cloud_init_iso(&instance)?;

        // Step 4: Start QEMU.
        Self::report_step(progress, "Starting QEMU...");
        instance.state = VmState::Starting;
        let child = self.start_qemu(&instance)?;
        instance.process = Some(child);

        // Step 5: Wait for SSH readiness.
        Self::report_step(progress, "Booting VM (waiting for SSH)...");
        self.wait_for_ssh(&instance)?;

        // Step 6: Wait for cloud-init to finish (package installation, user setup).
        Self::report_step(progress, "Installing packages (cloud-init)...");
        self.wait_for_cloud_init(&instance)?;

        // Step 7: Sync host repositories into the VM.
        if !repo_paths.is_empty() {
            Self::report_step(progress, "Syncing repositories into VM...");
        }
        self.sync_repos(&instance, repo_paths)?;

        Self::report_step(progress, "Connecting...");
        instance.state = VmState::Ready;
        info!(vm_id = %vm_id, "VM is ready");

        self.instances
            .lock()
            .unwrap()
            .insert(vm_id.to_string(), instance);
        Ok(())
    }

    /// Generate an ephemeral SSH keypair for VM access.
    fn generate_ssh_keypair(&self, instance: &VmInstance) -> Result<()> {
        let key_path = instance.ssh_key_path();
        let output = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &key_path.to_string_lossy(),
                "-N",
                "", // No passphrase
                "-C",
                &format!("thurbox-vm-{}", instance.id),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to run ssh-keygen")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("ssh-keygen failed: {}", stderr.trim());
        }

        debug!(vm_id = %instance.id, "SSH keypair generated");
        Ok(())
    }

    /// Create a CoW overlay disk backed by the base image.
    fn create_overlay_disk(&self, instance: &VmInstance) -> Result<()> {
        let base_path = self.base_image_path(&instance.config.base_image);
        if !base_path.exists() {
            bail!(
                "Base image not found: {} (this is a bug — ensure_base_image should have downloaded it)",
                instance.config.base_image,
            );
        }

        let disk_path = instance.disk_path();
        let output = Command::new("qemu-img")
            .args([
                "create",
                "-f",
                "qcow2",
                "-b",
                &base_path.to_string_lossy(),
                "-F",
                "qcow2",
                &disk_path.to_string_lossy(),
                &format!("{}G", instance.config.disk_gb),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to run qemu-img")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("qemu-img create failed: {}", stderr.trim());
        }

        debug!(vm_id = %instance.id, "Overlay disk created");
        Ok(())
    }

    /// Generate a cloud-init nocloud ISO for VM provisioning.
    fn create_cloud_init_iso(&self, instance: &VmInstance) -> Result<()> {
        let pub_key_path = format!("{}.pub", instance.ssh_key_path().display());
        let pub_key =
            std::fs::read_to_string(&pub_key_path).context("Failed to read SSH public key")?;

        let user_data = format!(
            r#"#cloud-config
users:
  - name: {user}
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    ssh_authorized_keys:
      - {pub_key}
package_update: true
packages:
  - tmux
  - rsync
  - git
  - curl
runcmd:
  - su - {user} -c 'curl -fsSL https://claude.ai/install.sh | bash'
  - ln -sf /home/{user}/.local/bin/claude /usr/local/bin/claude
{extra_runcmd}
"#,
            user = instance.ssh_user(),
            pub_key = pub_key.trim(),
            extra_runcmd = instance
                .config
                .setup_script
                .as_ref()
                .map(|s| format!("  - |\n    {}", s.replace('\n', "\n    ")))
                .unwrap_or_default(),
        );

        let meta_data = format!(
            r#"instance-id: thurbox-{id}
local-hostname: thurbox-vm
"#,
            id = instance.id,
        );

        // Write temporary files for ISO creation.
        let ci_dir = instance.vm_dir.join("cloud-init-src");
        std::fs::create_dir_all(&ci_dir)?;
        std::fs::write(ci_dir.join("user-data"), user_data)?;
        std::fs::write(ci_dir.join("meta-data"), meta_data)?;

        let iso_path = instance.cloud_init_path();

        // Try genisoimage first, fall back to mkisofs.
        let iso_tool = if Command::new("genisoimage")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
        {
            "genisoimage"
        } else {
            "mkisofs"
        };

        let output = Command::new(iso_tool)
            .args([
                "-output",
                &iso_path.to_string_lossy(),
                "-volid",
                "cidata",
                "-joliet",
                "-rock",
                &ci_dir.to_string_lossy(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .context(format!("Failed to run {iso_tool}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("{iso_tool} failed: {}", stderr.trim());
        }

        // Clean up temp dir.
        let _ = std::fs::remove_dir_all(&ci_dir);

        debug!(vm_id = %instance.id, "Cloud-init ISO created");
        Ok(())
    }

    /// Start the QEMU process.
    fn start_qemu(&self, instance: &VmInstance) -> Result<Child> {
        let child = Command::new("qemu-system-x86_64")
            .args([
                "-enable-kvm",
                "-cpu",
                "host",
                "-nographic",
                "-m",
                &format!("{}M", instance.config.memory_mb),
                "-smp",
                &instance.config.cpus.to_string(),
                "-drive",
                &format!(
                    "file={},format=qcow2,if=virtio",
                    instance.disk_path().display()
                ),
                "-cdrom",
                &instance.cloud_init_path().to_string_lossy(),
                "-netdev",
                &format!("user,id=net0,hostfwd=tcp::{}-:22", instance.ssh_port),
                "-device",
                "virtio-net-pci,netdev=net0",
                "-pidfile",
                &instance.vm_dir.join("qemu.pid").to_string_lossy(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to start QEMU")?;

        debug!(
            vm_id = %instance.id,
            pid = child.id(),
            "QEMU process started"
        );

        Ok(child)
    }

    /// Poll SSH until the VM is reachable.
    fn wait_for_ssh(&self, instance: &VmInstance) -> Result<()> {
        let start = Instant::now();
        let key_path = instance.ssh_key_path();

        loop {
            if start.elapsed() > VM_BOOT_TIMEOUT {
                bail!(
                    "VM {} did not become SSH-ready within {}s",
                    instance.id,
                    VM_BOOT_TIMEOUT.as_secs()
                );
            }

            let status = Command::new("ssh")
                .args([
                    "-i",
                    &key_path.to_string_lossy(),
                    "-o",
                    "StrictHostKeyChecking=no",
                    "-o",
                    "UserKnownHostsFile=/dev/null",
                    "-o",
                    &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
                    "-o",
                    "BatchMode=yes",
                    "-p",
                    &instance.ssh_port.to_string(),
                    &format!("{}@localhost", instance.ssh_user()),
                    "true",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            match status {
                Ok(s) if s.success() => {
                    debug!(
                        vm_id = %instance.id,
                        elapsed_ms = start.elapsed().as_millis(),
                        "SSH is ready"
                    );
                    return Ok(());
                }
                _ => {
                    std::thread::sleep(SSH_POLL_INTERVAL);
                }
            }
        }
    }

    /// Wait for cloud-init to finish provisioning (package installs, user setup).
    ///
    /// SSH can become available before cloud-init completes. Without this wait,
    /// tools like tmux and rsync may not be installed yet.
    fn wait_for_cloud_init(&self, instance: &VmInstance) -> Result<()> {
        let key_path = instance.ssh_key_path();
        info!(vm_id = %instance.id, "Waiting for cloud-init to finish...");

        let output = Command::new("ssh")
            .args([
                "-i",
                &key_path.to_string_lossy(),
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "BatchMode=yes",
                "-p",
                &instance.ssh_port.to_string(),
                &format!("{}@localhost", instance.ssh_user()),
                "cloud-init",
                "status",
                "--wait",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to run cloud-init status --wait")?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // cloud-init may report "status: error" if a runcmd fails,
            // but packages are usually installed by then. Log and continue.
            warn!(
                vm_id = %instance.id,
                stdout = %stdout.trim(),
                stderr = %stderr.trim(),
                "cloud-init finished with non-zero exit"
            );
        } else {
            debug!(
                vm_id = %instance.id,
                status = %stdout.trim(),
                "cloud-init finished"
            );
        }

        Ok(())
    }

    /// Build SSH command arguments for the control mode connection to a VM.
    ///
    /// Does NOT use SSH multiplexing (ControlMaster/ControlPersist) because
    /// the control mode connection must keep the foreground SSH process alive
    /// with stdin/stdout pipes open.
    pub fn ssh_args(&self, instance: &VmInstance) -> Vec<String> {
        vec![
            "-i".to_string(),
            instance.ssh_key_path().to_string_lossy().to_string(),
            "-o".to_string(),
            "StrictHostKeyChecking=no".to_string(),
            "-o".to_string(),
            "UserKnownHostsFile=/dev/null".to_string(),
            "-o".to_string(),
            "LogLevel=ERROR".to_string(),
            "-o".to_string(),
            "ServerAliveInterval=30".to_string(),
            "-o".to_string(),
            "ServerAliveCountMax=3".to_string(),
            "-p".to_string(),
            instance.ssh_port.to_string(),
            format!("{}@localhost", instance.ssh_user()),
        ]
    }

    /// Stop a running VM gracefully (send poweroff via SSH, then wait).
    pub fn stop_vm(&self, vm_id: &str) -> Result<()> {
        let mut instances = self.instances.lock().unwrap();
        let instance = instances
            .get_mut(vm_id)
            .context(format!("VM {vm_id} not found"))?;

        if instance.state == VmState::Stopped {
            return Ok(());
        }

        instance.state = VmState::Stopping;
        info!(vm_id = %vm_id, "Stopping VM");

        // Try graceful shutdown via SSH first.
        let _ = Command::new("ssh")
            .args([
                "-i",
                &instance.ssh_key_path().to_string_lossy(),
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
                "-o",
                "BatchMode=yes",
                "-p",
                &instance.ssh_port.to_string(),
                &format!("{}@localhost", instance.ssh_user()),
                "sudo",
                "poweroff",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // Wait for QEMU to exit (with timeout).
        if let Some(ref mut child) = instance.process {
            match child.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    // Give it a few seconds to shut down gracefully.
                    std::thread::sleep(Duration::from_secs(5));
                    match child.try_wait() {
                        Ok(Some(_)) => {}
                        _ => {
                            warn!(vm_id = %vm_id, "QEMU did not exit gracefully, killing");
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                    }
                }
            }
        }

        instance.state = VmState::Stopped;
        instance.process = None;
        info!(vm_id = %vm_id, "VM stopped");
        Ok(())
    }

    /// Destroy a VM — stop it and remove all files.
    pub fn destroy_vm(&self, vm_id: &str) -> Result<()> {
        // Stop first if running.
        let _ = self.stop_vm(vm_id);

        let mut instances = self.instances.lock().unwrap();
        if let Some(instance) = instances.remove(vm_id) {
            // Remove VM directory and all files.
            if instance.vm_dir.exists() {
                std::fs::remove_dir_all(&instance.vm_dir)
                    .context("Failed to remove VM directory")?;
            }
            info!(vm_id = %vm_id, "VM destroyed");
        }

        Ok(())
    }

    /// Get the state of a VM.
    pub fn vm_state(&self, vm_id: &str) -> Option<VmState> {
        self.instances
            .lock()
            .unwrap()
            .get(vm_id)
            .map(|i| i.state.clone())
    }

    /// Get the SSH port of a VM.
    pub fn vm_ssh_port(&self, vm_id: &str) -> Option<u16> {
        self.instances
            .lock()
            .unwrap()
            .get(vm_id)
            .map(|i| i.ssh_port)
    }

    /// Re-register a running VM from its database record.
    ///
    /// Called during startup to restore VM instances that survived a Thurbox restart.
    /// Probes SSH to verify the VM is still running. If SSH succeeds, the instance
    /// is inserted into the in-memory map with state `Ready`. If SSH fails, returns
    /// an error (caller should mark the VM as stopped in the DB).
    pub fn restore_vm(&self, vm_record: &crate::storage::vms::VmRecord) -> Result<()> {
        let vm_dir = self.vms_dir.join(&vm_record.id);
        if !vm_dir.exists() {
            bail!("VM directory does not exist: {}", vm_dir.display());
        }

        let config = VmConfig {
            base_image: vm_record.base_image.clone(),
            cpus: vm_record.cpus,
            memory_mb: vm_record.memory_mb,
            disk_gb: vm_record.disk_gb,
            setup_script: None,
        };

        let instance = VmInstance {
            id: vm_record.id.clone(),
            state: VmState::Ready,
            ssh_port: vm_record.ssh_port,
            config,
            vm_dir,
            process: None, // We don't own the QEMU process handle after restart
        };

        // Probe SSH to verify the VM is still reachable.
        let key_path = instance.ssh_key_path();
        let status = Command::new("ssh")
            .args([
                "-i",
                &key_path.to_string_lossy(),
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
                "-o",
                "BatchMode=yes",
                "-o",
                "LogLevel=ERROR",
                "-p",
                &instance.ssh_port.to_string(),
                &format!("{}@localhost", instance.ssh_user()),
                "true",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        match status {
            Ok(s) if s.success() => {
                info!(
                    vm_id = %vm_record.id,
                    ssh_port = vm_record.ssh_port,
                    "VM restored — SSH is reachable"
                );
                // Advance the port allocator past this VM's port to avoid collisions.
                let mut next = self.next_port.lock().unwrap();
                if vm_record.ssh_port >= *next {
                    *next = vm_record.ssh_port + 1;
                }
                self.instances
                    .lock()
                    .unwrap()
                    .insert(vm_record.id.clone(), instance);
                Ok(())
            }
            _ => {
                bail!(
                    "VM {} is not reachable via SSH on port {}",
                    vm_record.id,
                    vm_record.ssh_port
                );
            }
        }
    }

    /// Get a reference to a VM instance by ID.
    pub fn get_instance(&self, vm_id: &str) -> Option<VmInstance> {
        // Return a clone-like snapshot (VmInstance is not Clone due to Child,
        // so we reconstruct the parts we need).
        let instances = self.instances.lock().unwrap();
        let inst = instances.get(vm_id)?;
        Some(VmInstance {
            id: inst.id.clone(),
            state: inst.state.clone(),
            ssh_port: inst.ssh_port,
            config: inst.config.clone(),
            vm_dir: inst.vm_dir.clone(),
            process: None, // Cannot clone the child process handle
        })
    }
}

// ---------------------------------------------------------------------------
// SshControlMode — tmux control mode over SSH
// ---------------------------------------------------------------------------

/// Tmux control mode connection tunneled through SSH to a VM.
///
/// Mirrors `ControlMode` in `tmux.rs` but spawns:
/// ```text
/// ssh <vm-ssh-args> tmux -L thurbox -C new-session -A -s thurbox
/// ```
/// instead of a local tmux attach.
struct SshControlMode {
    stdin: Arc<Mutex<ChildStdin>>,
    pane_senders: PaneSendersMapShared,
    response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    child: Mutex<Child>,
}

/// Timeout for waiting for a control mode command response.
const SSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Tmux socket name inside the VM.
const VM_TMUX_SOCKET: &str = "thurbox";

/// Tmux session name inside the VM.
const VM_TMUX_SESSION: &str = "thurbox";

impl SshControlMode {
    /// Start a control mode connection to a VM via SSH.
    fn start(ssh_args: &[String]) -> Result<Self> {
        let mut cmd = Command::new("ssh");
        // Force PTY allocation — without it, tmux's default window shell has no
        // PTY and exits immediately, causing tmux to send %exit and close control
        // mode.
        cmd.arg("-tt");
        for arg in ssh_args {
            cmd.arg(arg);
        }
        // Start tmux in control mode inside the VM.
        // -A = attach or create, -s = session name.
        cmd.args([
            "tmux",
            "-L",
            VM_TMUX_SOCKET,
            "-C",
            "new-session",
            "-A",
            "-s",
            VM_TMUX_SESSION,
        ]);

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to start SSH control mode to VM")?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to get SSH control mode stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to get SSH control mode stdout")?;

        let stdin = Arc::new(Mutex::new(stdin));
        let pane_senders: PaneSendersMapShared = Arc::new(Mutex::new(HashMap::new()));
        let response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>> =
            Arc::new(Mutex::new(VecDeque::new()));

        let reader_stdin = Arc::clone(&stdin);
        let reader_pane_senders = Arc::clone(&pane_senders);
        let reader_queue = Arc::clone(&response_queue);

        let reader_handle = std::thread::Builder::new()
            .name("ssh-control-reader".into())
            .spawn(move || {
                Self::reader_thread(stdout, reader_stdin, reader_pane_senders, reader_queue);
            })
            .context("Failed to spawn SSH control reader thread")?;

        let control = Self {
            stdin,
            pane_senders,
            response_queue,
            reader_handle: Mutex::new(Some(reader_handle)),
            child: Mutex::new(child),
        };

        // When tmux creates a new session (fresh VM), it emits an initial
        // %begin/%end block for `new-session`. Let the reader thread process
        // and discard it (no sender in the queue) before we send any commands.
        std::thread::sleep(Duration::from_secs(2));

        // Drain any remaining initial output. Retry once if the initial
        // session-creation response stole our sender (race condition).
        if control.send_command("refresh-client").is_err() {
            debug!("Initial refresh-client timed out (expected on fresh VMs), retrying");
            control.send_command("refresh-client")?;
        }

        // Enable flow control.
        control.send_command("refresh-client -f pause-after=5")?;

        // Apply VM-side tmux config.
        // Use -g for options that must apply to windows created after this point.
        control.send_command("set-option -g remain-on-exit on")?;
        control.send_command("set-option -g status off")?;
        control.send_command("set-option -g history-limit 5000")?;
        // NOTE: window-size manual crashes tmux 3.4 (Ubuntu 24.04 default).
        // The local backend uses tmux 3.6a where this works fine, but the VM
        // image ships 3.4. Omit it — tmux will use its default sizing.
        control.send_command("set-option -sg default-terminal xterm-256color")?;
        control.send_command("set-option -sg extended-keys on")?;

        Ok(control)
    }

    /// Background reader thread — identical logic to `ControlMode::reader_thread`.
    fn reader_thread(
        stdout: std::process::ChildStdout,
        stdin: Arc<Mutex<ChildStdin>>,
        pane_senders: PaneSendersMapShared,
        response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>>,
    ) {
        let mut reader = BufReader::new(stdout);
        let mut collecting: Option<Vec<String>> = None;
        let mut line_buf = Vec::new();

        loop {
            line_buf.clear();
            match reader.read_until(b'\n', &mut line_buf) {
                Ok(0) => {
                    debug!("SSH control reader: EOF on stdout");
                    break;
                }
                Ok(n) => {
                    let preview = String::from_utf8_lossy(&line_buf);
                    debug!(bytes = n, line = %preview.trim(), "SSH control reader: line received");
                }
                Err(e) => {
                    debug!("SSH control reader I/O error: {e}");
                    break;
                }
            }
            if line_buf.last() == Some(&b'\n') {
                line_buf.pop();
            }
            // SSH with -tt uses a PTY which produces \r\n line endings.
            if line_buf.last() == Some(&b'\r') {
                line_buf.pop();
            }
            let line = String::from_utf8_lossy(&line_buf);

            match control_mode::parse_notification(&line) {
                Notification::Output { pane_id, data } => {
                    if let Ok(senders) = pane_senders.lock() {
                        if let Some(tx_vec) = senders.get(&pane_id) {
                            for tx in tx_vec {
                                match tx.try_send(data.clone()) {
                                    Ok(()) => {}
                                    Err(std::sync::mpsc::TrySendError::Full(_)) => {
                                        debug!(
                                            pane_id = %pane_id,
                                            "VM pane output channel full, dropping chunk"
                                        );
                                    }
                                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
                                }
                            }
                        }
                    }
                }
                Notification::Begin => {
                    collecting = Some(Vec::new());
                }
                end_or_error @ (Notification::End | Notification::Error) => {
                    let lines = collecting.take().unwrap_or_default();
                    if let Ok(mut queue) = response_queue.lock() {
                        if let Some(tx) = queue.pop_front() {
                            let _ = tx.send(CommandResponse {
                                lines,
                                is_error: matches!(end_or_error, Notification::Error),
                            });
                        }
                    }
                }
                Notification::Pause { pane_id } => {
                    let cmd = format!(
                        "refresh-client -A '{}:continue'\n",
                        pane_id.replace('\'', "'\\''")
                    );
                    if let Ok(mut s) = stdin.lock() {
                        let _ = s.write_all(cmd.as_bytes());
                        let _ = s.flush();
                    }
                }
                Notification::Other(text) => {
                    if let Some(ref mut lines) = collecting {
                        lines.push(text);
                    }
                }
            }
        }

        debug!("SSH control reader thread exiting");
        if let Ok(mut senders) = pane_senders.lock() {
            senders.clear();
        }
    }

    /// Send a command and wait for its response.
    fn send_command(&self, cmd: &str) -> Result<String> {
        let (tx, rx) = sync_channel(1);

        {
            let mut queue = self
                .response_queue
                .lock()
                .map_err(|e| anyhow::anyhow!("response_queue lock: {e}"))?;
            queue.push_back(tx);
        }

        {
            let mut stdin = self
                .stdin
                .lock()
                .map_err(|e| anyhow::anyhow!("stdin lock: {e}"))?;
            writeln!(stdin, "{cmd}")?;
            stdin.flush()?;
        }

        let response = rx
            .recv_timeout(SSH_COMMAND_TIMEOUT)
            .context(format!("Timeout waiting for VM tmux response to: {cmd}"))?;

        if response.is_error {
            bail!(
                "VM tmux command failed: {cmd}: {}",
                response.lines.join("\n")
            );
        }

        Ok(response.lines.join("\n"))
    }

    /// Send a command without waiting for a response.
    fn send_command_nowait(&self, cmd: &str) -> Result<()> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|e| anyhow::anyhow!("stdin lock: {e}"))?;
        writeln!(stdin, "{cmd}")?;
        stdin.flush()?;
        Ok(())
    }
}

impl Drop for SshControlMode {
    fn drop(&mut self) {
        if let Ok(mut stdin) = self.stdin.lock() {
            let _ = writeln!(stdin, "detach-client");
            let _ = stdin.flush();
        }
        if let Ok(mut handle) = self.reader_handle.lock() {
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
        if let Ok(mut child) = self.child.lock() {
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// QemuVmBackend — SessionBackend for QEMU/KVM VMs
// ---------------------------------------------------------------------------

/// Session backend that runs sessions inside QEMU/KVM virtual machines.
///
/// Each VM gets its own tmux server (accessed via SSH control mode).
/// The backend manages the mapping from VM IDs to SSH control mode connections.
pub struct QemuVmBackend {
    /// VM lifecycle manager (shared with app layer for provisioning).
    manager: Arc<Mutex<VmManager>>,
    /// Active SSH control mode connections, keyed by VM ID.
    controls: Mutex<HashMap<String, SshControlMode>>,
}

impl QemuVmBackend {
    /// Create a new VM backend.
    pub fn new(manager: Arc<Mutex<VmManager>>) -> Self {
        Self {
            manager,
            controls: Mutex::new(HashMap::new()),
        }
    }

    /// Ensure an SSH control mode connection exists for the given VM.
    fn ensure_control(&self, vm_id: &str) -> Result<()> {
        let mut controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;

        if controls.contains_key(vm_id) {
            return Ok(());
        }

        let manager = self
            .manager
            .lock()
            .map_err(|e| anyhow::anyhow!("manager lock: {e}"))?;

        let instance = manager
            .get_instance(vm_id)
            .context(format!("VM {vm_id} not found"))?;

        if instance.state != VmState::Ready {
            bail!("VM {vm_id} is not ready (state: {})", instance.state);
        }

        let ssh_args = manager.ssh_args(&instance);
        drop(manager); // Release lock before starting SSH (may take time)

        let control = SshControlMode::start(&ssh_args)?;
        controls.insert(vm_id.to_string(), control);

        debug!(vm_id = %vm_id, "SSH control mode established");
        Ok(())
    }

    /// Get the VM ID for a backend_id (format: "vm:<vm_id>:<pane_id>").
    fn parse_backend_id(backend_id: &str) -> Result<(&str, &str)> {
        let parts: Vec<&str> = backend_id.splitn(3, ':').collect();
        if parts.len() != 3 || parts[0] != "vm" {
            bail!("Invalid VM backend_id format: {backend_id} (expected vm:<vm_id>:<pane_id>)");
        }
        Ok((parts[1], parts[2]))
    }

    /// Build a composite backend_id: "vm:<vm_id>:<pane_id>".
    fn make_backend_id(vm_id: &str, pane_id: &str) -> String {
        format!("vm:{vm_id}:{pane_id}")
    }

    /// Run a tmux command on a specific VM's control mode.
    fn ctrl_command(&self, vm_id: &str, cmd: &str) -> Result<String> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        let ctrl = controls
            .get(vm_id)
            .context(format!("No control mode for VM {vm_id}"))?;
        ctrl.send_command(cmd)
    }

    /// Run a tmux command without waiting for response.
    fn ctrl_command_nowait(&self, vm_id: &str, cmd: &str) -> Result<()> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        let ctrl = controls
            .get(vm_id)
            .context(format!("No control mode for VM {vm_id}"))?;
        ctrl.send_command_nowait(cmd)
    }

    /// Register a pane sender for output monitoring.
    fn register_pane(&self, vm_id: &str, pane_id: &str) -> Result<ControlModeReader> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        let ctrl = controls
            .get(vm_id)
            .context(format!("No control mode for VM {vm_id}"))?;
        let (tx, rx) = sync_channel(PANE_CHANNEL_CAPACITY);
        {
            let mut senders = ctrl
                .pane_senders
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
            senders.entry(pane_id.to_string()).or_default().push(tx);
        }
        Ok(ControlModeReader::new(rx))
    }

    /// Unregister a pane sender.
    fn unregister_pane(&self, vm_id: &str, pane_id: &str) -> Result<()> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        if let Some(ctrl) = controls.get(vm_id) {
            let mut senders = ctrl
                .pane_senders
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
            senders.remove(pane_id);
        }
        Ok(())
    }

    /// Create a writer for a pane on a specific VM.
    fn pane_writer(&self, vm_id: &str, pane_id: &str) -> Result<ControlModeWriter> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        let ctrl = controls
            .get(vm_id)
            .context(format!("No control mode for VM {vm_id}"))?;
        Ok(ControlModeWriter {
            stdin: Arc::clone(&ctrl.stdin),
            pane_id: pane_id.to_string(),
        })
    }

    /// Capture screen content from a pane.
    fn capture_pane(&self, vm_id: &str, pane_id: &str) -> Vec<u8> {
        let cmd = format!("capture-pane -t {pane_id} -p -e -S -");
        let content = self.ctrl_command(vm_id, &cmd).unwrap_or_default();
        debug!(
            vm_id = %vm_id,
            pane_id = %pane_id,
            bytes = content.len(),
            "capture-pane result"
        );
        content.into_bytes()
    }

    /// Connect I/O to an existing pane in a VM.
    fn connect_pane(
        &self,
        vm_id: &str,
        pane_id: &str,
        rows: u16,
        cols: u16,
    ) -> Result<AdoptedSession> {
        let reader = self.register_pane(vm_id, pane_id)?;
        self.ctrl_command(
            vm_id,
            &format!("refresh-client -A '{}:on'", pane_id.replace('\'', "'\\''")),
        )?;
        let initial_screen = self.capture_pane(vm_id, pane_id);
        let writer = self.pane_writer(vm_id, pane_id)?;

        // Force resize to trigger repaint.
        self.force_resize(vm_id, pane_id, rows, cols)?;

        Ok(AdoptedSession {
            output: Box::new(reader),
            input: Box::new(writer),
            initial_screen,
        })
    }

    /// Force a resize to trigger SIGWINCH in the VM's pane.
    fn force_resize(&self, vm_id: &str, pane_id: &str, rows: u16, cols: u16) -> Result<()> {
        if rows > 1 {
            self.do_resize(vm_id, pane_id, rows - 1, cols)?;
        } else {
            self.do_resize(vm_id, pane_id, rows + 1, cols)?;
        }
        self.do_resize(vm_id, pane_id, rows, cols)?;
        Ok(())
    }

    /// Resize a pane inside a VM's tmux.
    fn do_resize(&self, vm_id: &str, pane_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.ctrl_command(
            vm_id,
            &format!("resize-window -t {pane_id} -x {cols} -y {rows}"),
        )?;
        self.ctrl_command(
            vm_id,
            &format!("resize-pane -t {pane_id} -x {cols} -y {rows}"),
        )?;
        Ok(())
    }

    /// Build a shell command string (same logic as LocalTmuxBackend).
    fn build_shell_command(command: &str, args: &[String]) -> String {
        let mut parts = vec![command.to_string()];
        for arg in args {
            parts.push(control_mode::shell_escape(arg));
        }
        parts.join(" ")
    }

    /// Disconnect from a VM's control mode (called when VM is being destroyed).
    pub fn disconnect_vm(&self, vm_id: &str) {
        if let Ok(mut controls) = self.controls.lock() {
            controls.remove(vm_id);
        }
    }
}

impl SessionBackend for QemuVmBackend {
    fn name(&self) -> &str {
        "qemu-vm"
    }

    fn check_available(&self) -> Result<()> {
        VmManager::check_available()
    }

    fn ensure_ready(&self) -> Result<()> {
        let manager = self
            .manager
            .lock()
            .map_err(|e| anyhow::anyhow!("manager lock: {e}"))?;
        manager.ensure_dirs()
    }

    fn spawn(
        &self,
        window_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &HashMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<SpawnedSession> {
        // The target VM ID is passed via the env map (injected by Session::spawn).
        // This ensures each session is tied to its specific VM.
        let vm_id = env
            .get(crate::agent::backend::VM_ID_ENV_KEY)
            .cloned()
            .context("No VM ID in env — caller must set SessionConfig.vm_id")?;

        let shell_cmd = Self::build_shell_command(command, args);

        let cwd_part = match cwd {
            Some(dir) => format!(" -c {}", control_mode::shell_escape(&dir.to_string_lossy())),
            None => String::new(),
        };
        let cmd = format!(
            "new-window -t {VM_TMUX_SESSION} -n {window_name} -P -F '#{{pane_id}}'{cwd_part} {shell_cmd}"
        );
        let result = self.ctrl_command(&vm_id, &cmd)?;
        let pane_id = result.trim().to_string();

        debug!(vm_id = %vm_id, pane_id = %pane_id, "VM tmux window created");

        let connected = self.connect_pane(&vm_id, &pane_id, rows, cols)?;
        let composite_id = Self::make_backend_id(&vm_id, &pane_id);

        Ok(SpawnedSession {
            backend_id: composite_id,
            output: connected.output,
            input: connected.input,
            initial_screen: connected.initial_screen,
        })
    }

    fn adopt(&self, backend_id: &str, rows: u16, cols: u16) -> Result<AdoptedSession> {
        let (vm_id, pane_id) = Self::parse_backend_id(backend_id)?;
        self.ensure_control(vm_id)?;
        self.connect_pane(vm_id, pane_id, rows, cols)
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;

        let mut sessions = Vec::new();
        for (vm_id, ctrl) in controls.iter() {
            let result = match ctrl.send_command(&format!(
                "list-windows -t {VM_TMUX_SESSION} -F '#{{pane_id}}|#{{window_name}}|#{{pane_dead}}'"
            )) {
                Ok(r) => r,
                Err(_) => continue,
            };

            for line in result.lines() {
                let parts: Vec<&str> = line.splitn(3, '|').collect();
                if parts.len() < 3 {
                    continue;
                }

                let window_name = parts[1];
                if !window_name.starts_with("tb-") {
                    continue;
                }

                sessions.push(DiscoveredSession {
                    backend_id: Self::make_backend_id(vm_id, parts[0]),
                    name: window_name.to_string(),
                    is_alive: parts[2] != "1",
                });
            }
        }

        Ok(sessions)
    }

    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        let (vm_id, pane_id) = Self::parse_backend_id(backend_id)?;
        self.do_resize(vm_id, pane_id, rows, cols)
    }

    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        let (vm_id, pane_id) = Self::parse_backend_id(backend_id)?;
        let result = self.ctrl_command(
            vm_id,
            &format!("display-message -t {pane_id} -p '#{{pane_dead}}'"),
        )?;
        Ok(result.trim() == "1")
    }

    fn kill(&self, backend_id: &str) -> Result<()> {
        let (vm_id, pane_id) = Self::parse_backend_id(backend_id)?;
        let _ = self.unregister_pane(vm_id, pane_id);
        self.ctrl_command(vm_id, &format!("kill-pane -t {pane_id}"))?;
        Ok(())
    }

    fn detach(&self, backend_id: &str) -> Result<()> {
        let (vm_id, pane_id) = Self::parse_backend_id(backend_id)?;
        if let Err(e) = self.ctrl_command_nowait(
            vm_id,
            &format!("refresh-client -A '{}:off'", pane_id.replace('\'', "'\\''")),
        ) {
            warn!("Failed to disable VM output monitoring during detach: {e}");
        }
        let _ = self.unregister_pane(vm_id, pane_id);
        Ok(())
    }

    fn prepare_vm(&self, vm_id: &str) -> Result<()> {
        self.ensure_control(vm_id)
    }

    fn default_shell(&self) -> String {
        "/bin/bash".to_string()
    }

    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        let (vm_id, pane_id) = Self::parse_backend_id(backend_id)?;
        let result = self.ctrl_command(
            vm_id,
            &format!("display-message -t {pane_id} -p '#{{pane_pid}}'"),
        )?;
        Ok(result.trim().parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_to_vm_path_basic() {
        let result = host_to_vm_path(Path::new("/home/user/projects/my-app"));
        assert_eq!(result, PathBuf::from("/home/thurbox/repos/my-app"));
    }

    #[test]
    fn host_to_vm_path_trailing_slash() {
        // PathBuf strips trailing slashes, so this should still work.
        let result = host_to_vm_path(Path::new("/home/user/projects/my-app/"));
        assert_eq!(result, PathBuf::from("/home/thurbox/repos/my-app"));
    }

    #[test]
    fn host_to_vm_path_deep_nested() {
        let result = host_to_vm_path(Path::new("/a/b/c/d/repo-name"));
        assert_eq!(result, PathBuf::from("/home/thurbox/repos/repo-name"));
    }

    #[test]
    fn host_to_vm_path_root_fallback() {
        // Root path has no file_name, should use "repo" fallback.
        let result = host_to_vm_path(Path::new("/"));
        assert_eq!(result, PathBuf::from("/home/thurbox/repos/repo"));
    }

    #[test]
    fn host_to_vm_path_single_component() {
        let result = host_to_vm_path(Path::new("my-project"));
        assert_eq!(result, PathBuf::from("/home/thurbox/repos/my-project"));
    }

    #[test]
    fn vm_state_display() {
        assert_eq!(VmState::Provisioning.to_string(), "Provisioning");
        assert_eq!(VmState::Starting.to_string(), "Starting");
        assert_eq!(VmState::Ready.to_string(), "Ready");
        assert_eq!(VmState::Stopping.to_string(), "Stopping");
        assert_eq!(VmState::Stopped.to_string(), "Stopped");
        assert_eq!(
            VmState::Failed("disk full".to_string()).to_string(),
            "Failed: disk full"
        );
    }

    #[test]
    fn vm_state_db_roundtrip() {
        let states = vec![
            VmState::Provisioning,
            VmState::Starting,
            VmState::Ready,
            VmState::Stopping,
            VmState::Stopped,
            VmState::Failed("test error".to_string()),
        ];

        for state in states {
            let db_str = state.to_db_str();
            let parsed = VmState::from_db_str(&db_str);
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn vm_state_unknown_string_defaults_to_stopped() {
        assert_eq!(VmState::from_db_str("unknown"), VmState::Stopped);
    }

    #[test]
    fn vm_config_default() {
        let config = VmConfig::default();
        assert_eq!(config.cpus, 2);
        assert_eq!(config.memory_mb, 2048);
        assert_eq!(config.disk_gb, 10);
        assert!(config.setup_script.is_none());
        assert!(config.base_image.contains("debian"));
    }

    #[test]
    fn debian_image_url_format() {
        let url = debian_image_url("debian-13-genericcloud-amd64.qcow2");
        assert_eq!(
            url,
            "https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2"
        );
    }

    #[test]
    fn vm_instance_paths() {
        let instance = VmInstance {
            id: "test-vm".to_string(),
            state: VmState::Stopped,
            ssh_port: 22200,
            config: VmConfig::default(),
            vm_dir: PathBuf::from("/tmp/thurbox/vms/test-vm"),
            process: None,
        };

        assert_eq!(
            instance.disk_path(),
            PathBuf::from("/tmp/thurbox/vms/test-vm/disk.qcow2")
        );
        assert_eq!(
            instance.cloud_init_path(),
            PathBuf::from("/tmp/thurbox/vms/test-vm/cloud-init.iso")
        );
        assert_eq!(
            instance.ssh_key_path(),
            PathBuf::from("/tmp/thurbox/vms/test-vm/ssh_key")
        );
        assert_eq!(
            instance.ssh_control_path(),
            PathBuf::from("/tmp/thurbox-ssh-test-vm")
        );
        assert_eq!(instance.ssh_user(), "thurbox");
    }

    #[test]
    fn vm_manager_new() {
        let manager = VmManager::new(Path::new("/tmp/thurbox"));
        assert_eq!(manager.vms_dir, PathBuf::from("/tmp/thurbox/vms"));
        assert_eq!(manager.images_dir, PathBuf::from("/tmp/thurbox/images"));
    }

    #[test]
    fn vm_manager_base_image_path() {
        let manager = VmManager::new(Path::new("/data/thurbox"));
        assert_eq!(
            manager.base_image_path("ubuntu.img"),
            PathBuf::from("/data/thurbox/images/ubuntu.img")
        );
    }

    #[test]
    fn vm_manager_allocate_port_increments() {
        let manager = VmManager::new(Path::new("/tmp"));
        let p1 = manager.allocate_port();
        let p2 = manager.allocate_port();
        // Ports must be in the expected range and strictly increasing.
        // Exact values depend on which ports are free on the host.
        assert!(p1 >= BASE_SSH_PORT);
        assert!(p2 > p1);
    }

    #[test]
    fn vm_manager_ssh_args_format() {
        let manager = VmManager::new(Path::new("/tmp/thurbox"));
        let instance = VmInstance {
            id: "test".to_string(),
            state: VmState::Ready,
            ssh_port: 22200,
            config: VmConfig::default(),
            vm_dir: PathBuf::from("/tmp/thurbox/vms/test"),
            process: None,
        };
        let args = manager.ssh_args(&instance);
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"22200".to_string()));
        assert!(args.iter().any(|a| a.contains("thurbox@localhost")));
    }

    // --- QemuVmBackend tests ---

    #[test]
    fn backend_name() {
        let manager = Arc::new(Mutex::new(VmManager::new(Path::new("/tmp"))));
        let backend = QemuVmBackend::new(manager);
        assert_eq!(backend.name(), "qemu-vm");
    }

    #[test]
    fn parse_backend_id_valid() {
        let (vm_id, pane_id) = QemuVmBackend::parse_backend_id("vm:abc-123:%42").unwrap();
        assert_eq!(vm_id, "abc-123");
        assert_eq!(pane_id, "%42");
    }

    #[test]
    fn parse_backend_id_invalid_prefix() {
        assert!(QemuVmBackend::parse_backend_id("tmux:abc:%42").is_err());
    }

    #[test]
    fn parse_backend_id_missing_parts() {
        assert!(QemuVmBackend::parse_backend_id("vm:abc").is_err());
    }

    #[test]
    fn parse_backend_id_empty() {
        assert!(QemuVmBackend::parse_backend_id("").is_err());
    }

    #[test]
    fn make_backend_id_format() {
        let id = QemuVmBackend::make_backend_id("vm-uuid-1", "%5");
        assert_eq!(id, "vm:vm-uuid-1:%5");
    }

    #[test]
    fn backend_id_roundtrip() {
        let vm_id = "test-vm-abc";
        let pane_id = "%99";
        let composite = QemuVmBackend::make_backend_id(vm_id, pane_id);
        let (parsed_vm, parsed_pane) = QemuVmBackend::parse_backend_id(&composite).unwrap();
        assert_eq!(parsed_vm, vm_id);
        assert_eq!(parsed_pane, pane_id);
    }

    #[test]
    fn build_shell_command_simple() {
        let cmd = QemuVmBackend::build_shell_command("claude", &[]);
        assert_eq!(cmd, "claude");
    }

    #[test]
    fn build_shell_command_with_args() {
        let args = vec!["--resume".to_string(), "abc-123".to_string()];
        let cmd = QemuVmBackend::build_shell_command("claude", &args);
        assert_eq!(cmd, "claude --resume abc-123");
    }

    #[test]
    fn disconnect_vm_removes_control() {
        let manager = Arc::new(Mutex::new(VmManager::new(Path::new("/tmp"))));
        let backend = QemuVmBackend::new(manager);
        // Should not panic even if VM doesn't exist
        backend.disconnect_vm("nonexistent");
    }

    #[test]
    fn get_instance_returns_snapshot() {
        let manager = VmManager::new(Path::new("/tmp/thurbox"));
        // No instances yet
        assert!(manager.get_instance("nonexistent").is_none());
    }
}
