//! Container-based session backend using native Docker/Podman.
//!
//! Provides isolated session environments using Docker or Podman (auto-detected,
//! preferring Podman). Each session gets its own container with optional
//! firewall-restricted network egress.
//!
//! ## Container storage layout
//!
//! ```text
//! ~/.local/share/thurbox/containers/<container-uuid>/
//!   ...  (runtime state)
//! ```
//!
//! ## Containerfile templates
//!
//! User-editable templates live in `~/.local/share/thurbox/containerfiles/`.
//! Each template is a **folder** containing a `Containerfile` and any support
//! files (e.g. `init-firewall.sh`). The entire folder is used as the build
//! context.
//!
//! ```text
//! ~/.local/share/thurbox/containerfiles/
//!   default/
//!     Containerfile
//!     init-firewall.sh
//!   python/
//!     Containerfile
//!     requirements.txt
//! ```
//!
//! A `default/` template is seeded on first run. Users can add more folders
//! for different environments and select them via a picker in the TUI.
//!
//! ## Architecture
//!
//! - `ContainerManager` handles the container lifecycle (build, run, stop, destroy).
//! - `DockerExecControlMode` tunnels tmux control mode over `docker/podman exec`.
//! - `DevcontainerBackend` implements `SessionBackend` and wires everything together.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{sync_channel, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

use crate::agent::backend::{AdoptedSession, DiscoveredSession, SessionBackend, SpawnedSession};
use crate::agent::control_mode::{
    self, CommandResponse, ControlModeReader, ControlModeWriter, Notification,
    PaneSendersMapShared, PANE_CHANNEL_CAPACITY,
};
use crate::session::{ContainerConfig, ContainerState};

// ---------------------------------------------------------------------------
// Container runtime detection
// ---------------------------------------------------------------------------

/// Supported container runtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Podman,
    Docker,
}

impl ContainerRuntime {
    /// Return the CLI command name for this runtime.
    pub fn cmd(&self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
        }
    }
}

impl std::fmt::Display for ContainerRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.cmd())
    }
}

/// Auto-detect the available container runtime, preferring Podman over Docker.
pub fn detect_runtime() -> Result<ContainerRuntime> {
    // Try podman first
    let podman = Command::new("podman")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if let Ok(s) = podman {
        if s.success() {
            return Ok(ContainerRuntime::Podman);
        }
    }

    // Fall back to docker
    let docker = Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match docker {
        Ok(s) if s.success() => Ok(ContainerRuntime::Docker),
        _ => bail!("No container runtime found. Install Podman or Docker."),
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Directory inside the container where host repos are mounted.
const CONTAINER_REPOS_DIR: &str = "/workspaces";

/// Tmux socket name inside the container.
const DC_TMUX_SOCKET: &str = "thurbox";

/// Tmux session name inside the container.
const DC_TMUX_SESSION: &str = "thurbox";

/// Timeout for waiting for a control mode command response.
const DC_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Short timeout for readiness poll commands during control mode startup.
const DC_READINESS_POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// Interval between readiness poll attempts during control mode startup.
const DC_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Maximum number of readiness poll attempts before giving up.
const DC_READINESS_MAX_ATTEMPTS: u32 = 15;

/// Max attempts to wait for a stopped container to become running after `start`.
const CONTAINER_START_MAX_ATTEMPTS: u32 = 10;

/// Delay between readiness checks when starting a stopped container.
const CONTAINER_START_POLL_INTERVAL: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Embedded default devcontainer assets
// ---------------------------------------------------------------------------

/// Default Containerfile for thurbox container sessions.
///
/// Based on the claude-code devcontainer but stripped to essentials:
/// Node.js LTS base, tmux, git, iptables/ipset, claude-code, non-root user.
/// This content is used to seed the `default` template in the containerfiles dir.
pub const DEFAULT_CONTAINERFILE: &str = r#"FROM debian:bookworm-slim

# Install development tools and firewall tooling
RUN apt-get update && apt-get install -y --no-install-recommends \
  curl \
  ca-certificates \
  less \
  git \
  procps \
  sudo \
  tmux \
  iptables \
  ipset \
  iproute2 \
  dnsutils \
  aggregate \
  jq \
  rsync \
  && apt-get clean && rm -rf /var/lib/apt/lists/*

# Dedicated sandbox user with fixed UID/GID (no host UID mapping)
RUN groupadd -g 5000 thurbox && \
  useradd -m -s /bin/bash -u 5000 -g 5000 thurbox && \
  echo 'thurbox ALL=(ALL) NOPASSWD:ALL' >> /etc/sudoers
ENV DEVCONTAINER=true

# Create workspace directory
RUN mkdir -p /workspaces && chown thurbox:thurbox /workspaces

WORKDIR /workspaces

USER thurbox

# Install Claude Code via native installer
RUN curl -fsSL https://claude.ai/install.sh | bash
ENV PATH="/home/thurbox/.local/bin:${PATH}"

# Copy firewall script and allowlist
COPY init-firewall.sh allowlist.conf /usr/local/bin/
USER root
RUN chmod +x /usr/local/bin/init-firewall.sh && \
  echo "thurbox ALL=(root) NOPASSWD: /usr/local/bin/init-firewall.sh" > /etc/sudoers.d/thurbox-firewall && \
  chmod 0440 /etc/sudoers.d/thurbox-firewall
USER thurbox
"#;

/// Firewall init script adapted from claude-code.
///
/// Whitelists Anthropic API, GitHub IPs, npm, sentry, statsig, localhost, DNS.
/// Drops all other egress.
///
/// This script is always injected into the build context alongside the
/// Containerfile so the `COPY init-firewall.sh /usr/local/bin/` instruction
/// works. It is also seeded into the containerfiles directory for visibility.
pub const INIT_FIREWALL_SH: &str = r#"#!/bin/bash
set -euo pipefail
IFS=$'\n\t'

# ---------------------------------------------------------------------------
# Firewall allowlist — loaded from allowlist.conf alongside this script.
#
# Edit allowlist.conf to add/remove allowed domains, CIDRs, or special
# directives. Changes take effect the next time a container is built.
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ALLOWLIST="${SCRIPT_DIR}/allowlist.conf"

if [ ! -f "$ALLOWLIST" ]; then
    echo "ERROR: allowlist.conf not found at $ALLOWLIST"
    exit 1
fi

# Parse allowlist.conf into arrays
DOMAINS=()
CIDRS=()
FETCH_GITHUB=false

while IFS= read -r line || [ -n "$line" ]; do
    # Strip comments and whitespace
    line="${line%%#*}"
    line="$(echo "$line" | xargs)" 2>/dev/null || true
    [ -z "$line" ] && continue

    if [ "$line" = "@github" ]; then
        FETCH_GITHUB=true
    elif [[ "$line" =~ / ]]; then
        # CIDR notation
        CIDRS+=("$line")
    else
        # Domain name
        DOMAINS+=("$line")
    fi
done < "$ALLOWLIST"

# 1. Extract Docker DNS info BEFORE any flushing
DOCKER_DNS_RULES=$(iptables-save -t nat | grep "127\.0\.0\.11" || true)

# Flush existing rules and delete existing ipsets
iptables -F
iptables -X
iptables -t nat -F
iptables -t nat -X
iptables -t mangle -F
iptables -t mangle -X
ipset destroy allowed-domains 2>/dev/null || true

# 2. Selectively restore ONLY internal Docker DNS resolution
if [ -n "$DOCKER_DNS_RULES" ]; then
    echo "Restoring Docker DNS rules..."
    iptables -t nat -N DOCKER_OUTPUT 2>/dev/null || true
    iptables -t nat -N DOCKER_POSTROUTING 2>/dev/null || true
    echo "$DOCKER_DNS_RULES" | xargs -L 1 iptables -t nat
else
    echo "No Docker DNS rules to restore"
fi

# First allow DNS and localhost before any restrictions
iptables -A OUTPUT -p udp --dport 53 -j ACCEPT
iptables -A INPUT -p udp --sport 53 -j ACCEPT
iptables -A OUTPUT -p tcp --dport 22 -j ACCEPT
iptables -A INPUT -p tcp --sport 22 -m state --state ESTABLISHED -j ACCEPT
iptables -A INPUT -i lo -j ACCEPT
iptables -A OUTPUT -o lo -j ACCEPT

# Create ipset with CIDR support
ipset create allowed-domains hash:net

# Fetch GitHub meta information and aggregate + add their IP ranges
if [ "$FETCH_GITHUB" = true ]; then
    echo "Fetching GitHub IP ranges..."
    gh_ranges=$(curl -s https://api.github.com/meta)
    if [ -z "$gh_ranges" ]; then
        echo "WARNING: Failed to fetch GitHub IP ranges, skipping"
    else
        if echo "$gh_ranges" | jq -e '.web and .api and .git' >/dev/null 2>&1; then
            echo "Processing GitHub IPs..."
            while read -r cidr; do
                if [[ "$cidr" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}/[0-9]{1,2}$ ]]; then
                    ipset add allowed-domains "$cidr" 2>/dev/null || true
                fi
            done < <(echo "$gh_ranges" | jq -r '(.web + .api + .git)[]' | aggregate -q 2>/dev/null || echo "$gh_ranges" | jq -r '(.web + .api + .git)[]')
        fi
    fi
fi

# Add explicit CIDRs from allowlist
for cidr in "${CIDRS[@]+"${CIDRS[@]}"}"; do
    echo "Adding CIDR $cidr..."
    ipset add allowed-domains "$cidr" 2>/dev/null || true
done

# Resolve and add allowed domains
for domain in "${DOMAINS[@]+"${DOMAINS[@]}"}"; do
    echo "Resolving $domain..."
    ips=$(dig +noall +answer A "$domain" 2>/dev/null | awk '$4 == "A" {print $5}') || true
    if [ -n "$ips" ]; then
        while read -r ip; do
            if [[ "$ip" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]]; then
                ipset add allowed-domains "$ip" 2>/dev/null || true
            fi
        done < <(echo "$ips")
    else
        echo "WARNING: Failed to resolve $domain"
    fi
done

# Get host IP from default route
HOST_IP=$(ip route | grep default | cut -d" " -f3 || true)
if [ -n "$HOST_IP" ]; then
    HOST_NETWORK=$(echo "$HOST_IP" | sed "s/\.[0-9]*$/.0\/24/")
    echo "Host network detected as: $HOST_NETWORK"
    iptables -A INPUT -s "$HOST_NETWORK" -j ACCEPT
    iptables -A OUTPUT -d "$HOST_NETWORK" -j ACCEPT
fi

# Set default policies to DROP
iptables -P INPUT DROP
iptables -P FORWARD DROP
iptables -P OUTPUT DROP

# Allow established connections
iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT

# Allow only specific outbound traffic to allowed domains
iptables -A OUTPUT -m set --match-set allowed-domains dst -j ACCEPT

# Reject all other outbound traffic
iptables -A OUTPUT -j REJECT --reject-with icmp-admin-prohibited

echo "Firewall configuration complete"
"#;

/// Default allowlist for the firewall script.
///
/// Seeded into each template's `allowlist.conf`. Users edit this file to
/// control which domains/CIDRs the container can reach. One entry per line:
/// - Domain names are resolved at firewall init time (e.g. `api.anthropic.com`)
/// - CIDR ranges are added directly (e.g. `10.0.0.0/8`)
/// - `@github` fetches GitHub's published IP ranges from their API
/// - Lines starting with `#` are comments
pub const DEFAULT_ALLOWLIST: &str = "\
# Firewall allowlist — one entry per line.
# Domains are resolved at container start. CIDRs are added directly.
# @github fetches GitHub's published IP ranges from their API.

# Anthropic API
api.anthropic.com
statsig.anthropic.com
sentry.io
statsig.com

# GitHub (fetches IP ranges from https://api.github.com/meta)
@github

# Package registries
registry.npmjs.org
";

// ---------------------------------------------------------------------------
// Section 1: ContainerManager
// ---------------------------------------------------------------------------

/// A running or stopped container instance.
pub struct ContainerInstance {
    /// Unique container identifier (thurbox-side UUID).
    pub id: String,
    /// Current state.
    pub state: ContainerState,
    /// Docker/Podman container name (e.g. `thurbox-<uuid>`).
    pub docker_container_id: Option<String>,
    /// Configuration used to create this container.
    pub config: ContainerConfig,
    /// Host-side workspace directory.
    pub workspace_dir: PathBuf,
}

/// Manages container lifecycle: build, run, stop, destroy.
pub struct ContainerManager {
    containers_dir: PathBuf,
    containerfiles_dir: PathBuf,
    runtime: ContainerRuntime,
    instances: Mutex<HashMap<String, ContainerInstance>>,
}

impl ContainerManager {
    /// Create a new container manager.
    pub fn new(data_dir: &Path, containerfiles_dir: PathBuf, runtime: ContainerRuntime) -> Self {
        Self {
            containers_dir: data_dir.join("containers"),
            containerfiles_dir,
            runtime,
            instances: Mutex::new(HashMap::new()),
        }
    }

    /// Return the container runtime in use.
    pub fn runtime(&self) -> ContainerRuntime {
        self.runtime
    }

    /// Check if the container runtime is available.
    pub fn check_available(runtime: ContainerRuntime) -> Result<()> {
        let status = Command::new(runtime.cmd())
            .arg("info")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        match status {
            Ok(s) if s.success() => Ok(()),
            _ => bail!("{} not available or not running", runtime.cmd()),
        }
    }

    /// Ensure storage directories exist.
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.containers_dir)
            .context("Failed to create containers directory")?;
        Ok(())
    }

    /// Get the host PID of a container's init process (PID 1 inside the container).
    ///
    /// Uses `<runtime> inspect` to query the container's State.Pid. Returns `None`
    /// if the container is not running or the PID cannot be determined.
    pub fn container_host_pid(&self, container_id: &str) -> Option<u32> {
        let docker_id = {
            let instances = self.instances.lock().ok()?;
            instances
                .get(container_id)
                .and_then(|i| i.docker_container_id.clone())?
        };
        let output = Command::new(self.runtime.cmd())
            .args(["inspect", "-f", "{{.State.Pid}}", &docker_id])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let pid_str = String::from_utf8_lossy(&output.stdout);
        let pid: u32 = pid_str.trim().parse().ok()?;
        if pid == 0 {
            None
        } else {
            Some(pid)
        }
    }

    /// Get a reference to a container instance.
    pub fn get_instance(&self, container_id: &str) -> Option<ContainerInstance> {
        let instances = self.instances.lock().ok()?;
        instances.get(container_id).map(|inst| ContainerInstance {
            id: inst.id.clone(),
            state: inst.state.clone(),
            docker_container_id: inst.docker_container_id.clone(),
            config: inst.config.clone(),
            workspace_dir: inst.workspace_dir.clone(),
        })
    }

    /// Create and start a container using native docker/podman commands.
    ///
    /// Pipeline:
    /// 1. Locate the template folder (`containerfiles/<name>/`)
    /// 2. Build using the folder as the build context (contains Containerfile + support files)
    /// 3. `<runtime> run -d --name thurbox-<uuid> ... sleep infinity`
    /// 4. Copy repo into container (if provided)
    /// 5. Run firewall init (if enabled)
    /// 6. Verify tmux is available inside container
    pub fn create_container(
        &self,
        container_id: &str,
        config: &ContainerConfig,
        containerfile_name: &str,
        repo_path: Option<&Path>,
        progress: &Sender<String>,
    ) -> Result<()> {
        let rt = self.runtime.cmd();
        let container_name = format!("thurbox-{container_id}");
        let image_tag = container_name.clone();

        // 1. Locate the template folder (the folder IS the build context)
        let _ = progress.send("Preparing build context...".to_string());
        let template_dir = self.containerfiles_dir.join(containerfile_name);
        let containerfile = template_dir.join("Containerfile");
        if !containerfile.exists() {
            bail!(
                "Containerfile not found in template '{}': expected {}",
                containerfile_name,
                containerfile.display()
            );
        }

        // 2. Build the image using the template folder as the build context
        let _ = progress.send("Building container image...".to_string());
        let build_output = Command::new(rt)
            .args(["build", "-t", &image_tag, "-f", "Containerfile", "."])
            .current_dir(&template_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to run container build")?;

        if !build_output.status.success() {
            let stderr = String::from_utf8_lossy(&build_output.stderr);
            bail!("Container image build failed: {stderr}");
        }

        // 3. Run the container
        let _ = progress.send("Starting container...".to_string());
        let mut run_args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            container_name.clone(),
            "--cap-add".to_string(),
            "NET_ADMIN".to_string(),
            "--cap-add".to_string(),
            "NET_RAW".to_string(),
        ];

        // Resource limits
        run_args.push("--cpus".to_string());
        run_args.push(config.cpus.to_string());
        run_args.push("-m".to_string());
        run_args.push(format!("{}m", config.memory_mb));

        // Image and command
        run_args.push(image_tag);
        run_args.push("sleep".to_string());
        run_args.push("infinity".to_string());

        let run_output = Command::new(rt)
            .args(&run_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to run container")?;

        if !run_output.status.success() {
            let stderr = String::from_utf8_lossy(&run_output.stderr);
            bail!("Container run failed: {stderr}");
        }

        // Container name is predictable — use it as docker_container_id
        let docker_container_id = container_name;

        info!(
            container_id = %container_id,
            docker_id = %docker_container_id,
            "Container started"
        );

        // 4. Copy repo into container (isolated copy, not a bind mount)
        if let Some(repo) = repo_path {
            self.copy_repo_into_container(rt, &docker_container_id, container_id, repo, progress)?;
        }

        // 5. Run firewall init if enabled
        if config.firewall_enabled {
            let _ = progress.send("Configuring firewall...".to_string());
            let fw_status = Command::new(rt)
                .args([
                    "exec",
                    &docker_container_id,
                    "sudo",
                    "/usr/local/bin/init-firewall.sh",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .status();

            match fw_status {
                Ok(s) if s.success() => {
                    info!(container_id = %container_id, "Firewall configured");
                }
                Ok(s) => {
                    warn!(
                        container_id = %container_id,
                        exit_code = ?s.code(),
                        "Firewall init failed (container will run without firewall)"
                    );
                }
                Err(e) => {
                    warn!(
                        container_id = %container_id,
                        "Failed to run firewall init: {e}"
                    );
                }
            }
        }

        // 6. Verify tmux is available inside the container
        let _ = progress.send("Verifying tmux...".to_string());
        let tmux_check = Command::new(rt)
            .args(["exec", &docker_container_id, "tmux", "-V"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .context("Failed to check tmux in container")?;

        if !tmux_check.success() {
            bail!("tmux not available inside container");
        }

        // Determine workspace directory
        let workspace_dir = if let Some(repo) = repo_path {
            repo.to_path_buf()
        } else {
            self.containers_dir.join(container_id)
        };

        // Store instance
        let instance = ContainerInstance {
            id: container_id.to_string(),
            state: ContainerState::Ready,
            docker_container_id: Some(docker_container_id),
            config: config.clone(),
            workspace_dir,
        };

        let mut instances = self
            .instances
            .lock()
            .map_err(|e| anyhow::anyhow!("instances lock: {e}"))?;
        instances.insert(container_id.to_string(), instance);

        let _ = progress.send("Container ready".to_string());
        Ok(())
    }

    /// Copy a host repository into the container using `docker/podman cp`.
    fn copy_repo_into_container(
        &self,
        rt: &str,
        docker_container_id: &str,
        container_id: &str,
        repo: &Path,
        progress: &Sender<String>,
    ) -> Result<()> {
        let _ = progress.send("Copying repository into container...".to_string());
        let basename = repo
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("workspace"));
        let dest = PathBuf::from(CONTAINER_REPOS_DIR).join(basename);
        let dest_str = dest.display().to_string();

        // Create the destination directory
        let mkdir_status = Command::new(rt)
            .args(["exec", docker_container_id, "mkdir", "-p", &dest_str])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Ok(s) = mkdir_status {
            if !s.success() {
                warn!(container_id = %container_id, "Failed to create workspace dir in container");
            }
        }

        // Trailing /. copies contents of the directory into dest
        let mut src_dot = repo.to_path_buf();
        src_dot.push(".");
        let cp_output = Command::new(rt)
            .args([
                "cp",
                &src_dot.display().to_string(),
                &format!("{docker_container_id}:{dest_str}"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to copy repo into container")?;

        if !cp_output.status.success() {
            let stderr = String::from_utf8_lossy(&cp_output.stderr);
            bail!("Failed to copy repo into container: {stderr}");
        }

        // Ensure thurbox user owns the copied files
        let chown_status = Command::new(rt)
            .args([
                "exec",
                "--user",
                "root",
                docker_container_id,
                "chown",
                "-R",
                "thurbox:thurbox",
                &dest_str,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Ok(s) = chown_status {
            if !s.success() {
                warn!(container_id = %container_id, "Failed to chown workspace");
            }
        }

        info!(
            container_id = %container_id,
            repo = %repo.display(),
            "Repository copied into container"
        );
        Ok(())
    }

    /// Stop a running container.
    pub fn stop_container(&self, container_id: &str) -> Result<()> {
        let mut instances = self
            .instances
            .lock()
            .map_err(|e| anyhow::anyhow!("instances lock: {e}"))?;

        let instance = instances
            .get_mut(container_id)
            .context(format!("Container {container_id} not found"))?;

        if matches!(
            instance.state,
            ContainerState::Stopped | ContainerState::Stopping
        ) {
            return Ok(());
        }

        instance.state = ContainerState::Stopping;

        if let Some(ref docker_id) = instance.docker_container_id {
            let _ = Command::new(self.runtime.cmd())
                .args(["stop", docker_id])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }

        instance.state = ContainerState::Stopped;
        info!(container_id = %container_id, "Container stopped");
        Ok(())
    }

    /// Destroy a container (stop + remove).
    pub fn destroy_container(&self, container_id: &str) -> Result<()> {
        let docker_id = {
            let instances = self
                .instances
                .lock()
                .map_err(|e| anyhow::anyhow!("instances lock: {e}"))?;
            instances
                .get(container_id)
                .and_then(|i| i.docker_container_id.clone())
        };

        // Stop first
        let _ = self.stop_container(container_id);

        // Remove the container
        if let Some(ref docker_id) = docker_id {
            let _ = Command::new(self.runtime.cmd())
                .args(["rm", "-f", docker_id])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }

        // Remove from instances
        let mut instances = self
            .instances
            .lock()
            .map_err(|e| anyhow::anyhow!("instances lock: {e}"))?;
        instances.remove(container_id);

        info!(container_id = %container_id, "Container destroyed");
        Ok(())
    }

    /// Restore a container from a database record (on restart).
    ///
    /// If the container is stopped, starts it first. Re-registers it in the
    /// instance map once running.
    pub fn restore_container(
        &self,
        container_id: &str,
        docker_container_id: &str,
        config: &ContainerConfig,
        workspace_dir: &Path,
    ) -> Result<()> {
        // Check if the container exists and its state
        let output = Command::new(self.runtime.cmd())
            .args(["inspect", "-f", "{{.State.Status}}", docker_container_id])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .context("Failed to inspect container")?;

        if !output.status.success() {
            bail!("Container {docker_container_id} not found");
        }

        let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
        match status.as_str() {
            "running" => {}
            "exited" | "stopped" | "created" => {
                info!(
                    docker_id = %docker_container_id,
                    status = %status,
                    "Container is stopped, starting it"
                );
                let start = Command::new(self.runtime.cmd())
                    .args(["start", docker_container_id])
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .output()
                    .context("Failed to start stopped container")?;
                if !start.status.success() {
                    let stderr = String::from_utf8_lossy(&start.stderr);
                    bail!("Failed to start container {docker_container_id}: {stderr}");
                }

                // Wait for the container to become running
                for attempt in 0..CONTAINER_START_MAX_ATTEMPTS {
                    let check = Command::new(self.runtime.cmd())
                        .args(["inspect", "-f", "{{.State.Running}}", docker_container_id])
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null())
                        .output();
                    if let Ok(out) = check {
                        if String::from_utf8_lossy(&out.stdout).trim() == "true" {
                            break;
                        }
                    }
                    if attempt == CONTAINER_START_MAX_ATTEMPTS - 1 {
                        bail!("Container {docker_container_id} did not become running after start");
                    }
                    std::thread::sleep(CONTAINER_START_POLL_INTERVAL);
                }

                info!(docker_id = %docker_container_id, "Container restarted successfully");
            }
            _ => {
                bail!("Container {docker_container_id} is in unexpected state: {status}");
            }
        }

        let instance = ContainerInstance {
            id: container_id.to_string(),
            state: ContainerState::Ready,
            docker_container_id: Some(docker_container_id.to_string()),
            config: config.clone(),
            workspace_dir: workspace_dir.to_path_buf(),
        };

        let mut instances = self
            .instances
            .lock()
            .map_err(|e| anyhow::anyhow!("instances lock: {e}"))?;
        instances.insert(container_id.to_string(), instance);

        info!(
            container_id = %container_id,
            docker_id = %docker_container_id,
            "Container restored"
        );
        Ok(())
    }
}

/// Map a host path to the container workspace path.
///
/// Mirrors `host_to_vm_path` but uses the devcontainer workspace directory.
pub fn host_to_container_path(host_path: &Path) -> PathBuf {
    let basename = host_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("workspace"));
    PathBuf::from(CONTAINER_REPOS_DIR).join(basename)
}

// ---------------------------------------------------------------------------
// Section 2: DockerExecControlMode
// ---------------------------------------------------------------------------

/// Tmux control mode connection tunneled through `docker exec` into a container.
///
/// Mirrors `SshControlMode` in `vm.rs` but spawns:
/// ```text
/// docker exec -i <container_id> tmux -L thurbox -C new-session -A -s thurbox
/// ```
/// instead of SSH.
struct DockerExecControlMode {
    stdin: Arc<Mutex<ChildStdin>>,
    pane_senders: PaneSendersMapShared,
    response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>>,
    reader_handle: Mutex<Option<JoinHandle<()>>>,
    child: Mutex<Child>,
}

impl DockerExecControlMode {
    /// Start a control mode connection to a container via docker/podman exec.
    fn start(runtime: &str, docker_container_id: &str) -> Result<Self> {
        let mut cmd = Command::new(runtime);
        cmd.args([
            "exec",
            "-i",
            docker_container_id,
            "tmux",
            "-L",
            DC_TMUX_SOCKET,
            "-C",
            "new-session",
            "-A",
            "-s",
            DC_TMUX_SESSION,
        ]);

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to start docker exec control mode")?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to get docker exec stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to get docker exec stdout")?;

        let stdin = Arc::new(Mutex::new(stdin));
        let pane_senders: PaneSendersMapShared = Arc::new(Mutex::new(HashMap::new()));
        let response_queue: Arc<Mutex<VecDeque<SyncSender<CommandResponse>>>> =
            Arc::new(Mutex::new(VecDeque::new()));

        let reader_stdin = Arc::clone(&stdin);
        let reader_pane_senders = Arc::clone(&pane_senders);
        let reader_queue = Arc::clone(&response_queue);

        let reader_handle = std::thread::Builder::new()
            .name("dc-control-reader".into())
            .spawn(move || {
                Self::reader_thread(stdout, reader_stdin, reader_pane_senders, reader_queue);
            })
            .context("Failed to spawn docker exec control reader thread")?;

        let control = Self {
            stdin,
            pane_senders,
            response_queue,
            reader_handle: Mutex::new(Some(reader_handle)),
            child: Mutex::new(child),
        };

        // Poll until the container's tmux is responsive (typically 200-500ms,
        // replaces the old 2s blind sleep).
        let mut ready = false;
        for attempt in 1..=DC_READINESS_MAX_ATTEMPTS {
            match control.send_command_timeout("refresh-client", DC_READINESS_POLL_TIMEOUT) {
                Ok(_) => {
                    debug!(attempt, "Container tmux responsive after readiness poll");
                    ready = true;
                    break;
                }
                Err(e) => {
                    debug!(attempt, "Readiness poll attempt failed: {e}");
                    if attempt < DC_READINESS_MAX_ATTEMPTS {
                        std::thread::sleep(DC_READINESS_POLL_INTERVAL);
                    }
                }
            }
        }
        if !ready {
            bail!("Container tmux not responsive after {DC_READINESS_MAX_ATTEMPTS} readiness poll attempts");
        }

        // Enable flow control.
        control.send_command("refresh-client -f pause-after=5")?;

        // Apply container-side tmux config.
        control.send_command("set-option -g remain-on-exit on")?;
        control.send_command("set-option -g status off")?;
        control.send_command("set-option -g history-limit 5000")?;
        control.send_command("set-option -sg default-terminal xterm-256color")?;
        control.send_command("set-option -sg extended-keys on")?;

        Ok(control)
    }

    /// Background reader thread — identical logic to `SshControlMode::reader_thread`.
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
                    debug!("Docker exec control reader: EOF on stdout");
                    break;
                }
                Ok(n) => {
                    let preview = String::from_utf8_lossy(&line_buf);
                    debug!(bytes = n, line = %preview.trim(), "Docker exec control reader: line received");
                }
                Err(e) => {
                    debug!("Docker exec control reader I/O error: {e}");
                    break;
                }
            }
            if line_buf.last() == Some(&b'\n') {
                line_buf.pop();
            }
            // docker exec without -t doesn't add \r, but handle it defensively
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
                                            "Container pane output channel full, dropping chunk"
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

        debug!("Docker exec control reader thread exiting");
        if let Ok(mut senders) = pane_senders.lock() {
            senders.clear();
        }
    }

    /// Send a command and wait for its response.
    fn send_command(&self, cmd: &str) -> Result<String> {
        self.send_command_timeout(cmd, DC_COMMAND_TIMEOUT)
    }

    /// Send a command and wait for its response with a custom timeout.
    fn send_command_timeout(&self, cmd: &str, timeout: Duration) -> Result<String> {
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

        let response = rx.recv_timeout(timeout).context(format!(
            "Timeout waiting for container tmux response to: {cmd}"
        ))?;

        if response.is_error {
            bail!(
                "Container tmux command failed: {cmd}: {}",
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

impl Drop for DockerExecControlMode {
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
// Section 3: DevcontainerBackend
// ---------------------------------------------------------------------------

/// Session backend that runs sessions inside containers (Docker or Podman).
///
/// Each container gets its own tmux server (accessed via `exec` control mode).
/// The backend manages the mapping from container IDs to control mode connections.
pub struct DevcontainerBackend {
    /// Container lifecycle manager (shared with app layer for provisioning).
    manager: Arc<Mutex<ContainerManager>>,
    /// Container runtime (podman or docker).
    runtime: ContainerRuntime,
    /// Active exec control mode connections, keyed by container ID.
    controls: Mutex<HashMap<String, DockerExecControlMode>>,
}

impl DevcontainerBackend {
    /// Create a new devcontainer backend.
    pub fn new(manager: Arc<Mutex<ContainerManager>>, runtime: ContainerRuntime) -> Self {
        Self {
            manager,
            runtime,
            controls: Mutex::new(HashMap::new()),
        }
    }

    /// Ensure a docker exec control mode connection exists for the given container.
    ///
    /// Lock scoping is deliberate: the `controls` and `manager` mutexes are held
    /// only for the brief check/lookup, then released before the expensive
    /// `DockerExecControlMode::start()` call. This allows parallel restoration
    /// of multiple containers without serialising on the locks.
    fn ensure_control(&self, container_id: &str) -> Result<()> {
        // 1. Fast path — already connected (brief lock).
        {
            let controls = self
                .controls
                .lock()
                .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
            if controls.contains_key(container_id) {
                return Ok(());
            }
        }

        // 2. Look up the docker container ID (brief lock).
        let docker_id = {
            let manager = self
                .manager
                .lock()
                .map_err(|e| anyhow::anyhow!("manager lock: {e}"))?;

            let instance = manager
                .get_instance(container_id)
                .context(format!("Container {container_id} not found"))?;

            if instance.state != ContainerState::Ready {
                bail!(
                    "Container {container_id} is not ready (state: {})",
                    instance.state
                );
            }

            instance
                .docker_container_id
                .as_ref()
                .context("Container has no docker container ID")?
                .clone()
        };

        // 3. Start control mode (NO locks held — expensive I/O).
        let control = DockerExecControlMode::start(self.runtime.cmd(), &docker_id)?;

        // 4. Insert into the map (brief lock).
        {
            let mut controls = self
                .controls
                .lock()
                .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
            controls.insert(container_id.to_string(), control);
        }

        debug!(container_id = %container_id, "Docker exec control mode established");
        Ok(())
    }

    /// Get the container ID from a backend_id (format: "dc:<container_id>:<pane_id>").
    fn parse_backend_id(backend_id: &str) -> Result<(&str, &str)> {
        let parts: Vec<&str> = backend_id.splitn(3, ':').collect();
        if parts.len() != 3 || parts[0] != "dc" {
            bail!(
                "Invalid devcontainer backend_id format: {backend_id} (expected dc:<container_id>:<pane_id>)"
            );
        }
        Ok((parts[1], parts[2]))
    }

    /// Build a composite backend_id: "dc:<container_id>:<pane_id>".
    fn make_backend_id(container_id: &str, pane_id: &str) -> String {
        format!("dc:{container_id}:{pane_id}")
    }

    /// Run a tmux command on a specific container's control mode.
    fn ctrl_command(&self, container_id: &str, cmd: &str) -> Result<String> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        let ctrl = controls
            .get(container_id)
            .context(format!("No control mode for container {container_id}"))?;
        ctrl.send_command(cmd)
    }

    /// Run a tmux command without waiting for response.
    fn ctrl_command_nowait(&self, container_id: &str, cmd: &str) -> Result<()> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        let ctrl = controls
            .get(container_id)
            .context(format!("No control mode for container {container_id}"))?;
        ctrl.send_command_nowait(cmd)
    }

    /// Register a pane sender for output monitoring.
    fn register_pane(&self, container_id: &str, pane_id: &str) -> Result<ControlModeReader> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        let ctrl = controls
            .get(container_id)
            .context(format!("No control mode for container {container_id}"))?;
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
    fn unregister_pane(&self, container_id: &str, pane_id: &str) -> Result<()> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        if let Some(ctrl) = controls.get(container_id) {
            let mut senders = ctrl
                .pane_senders
                .lock()
                .map_err(|e| anyhow::anyhow!("pane_senders lock: {e}"))?;
            senders.remove(pane_id);
        }
        Ok(())
    }

    /// Create a writer for a pane on a specific container.
    fn pane_writer(&self, container_id: &str, pane_id: &str) -> Result<ControlModeWriter> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;
        let ctrl = controls
            .get(container_id)
            .context(format!("No control mode for container {container_id}"))?;
        Ok(ControlModeWriter {
            stdin: Arc::clone(&ctrl.stdin),
            pane_id: pane_id.to_string(),
        })
    }

    /// Capture screen content from a pane.
    fn capture_pane(&self, container_id: &str, pane_id: &str) -> Vec<u8> {
        let cmd = format!("capture-pane -t {pane_id} -p -e -S -");
        let content = self.ctrl_command(container_id, &cmd).unwrap_or_default();
        debug!(
            container_id = %container_id,
            pane_id = %pane_id,
            bytes = content.len(),
            "capture-pane result"
        );
        content.into_bytes()
    }

    /// Connect I/O to an existing pane in a container.
    fn connect_pane(
        &self,
        container_id: &str,
        pane_id: &str,
        rows: u16,
        cols: u16,
    ) -> Result<AdoptedSession> {
        let reader = self.register_pane(container_id, pane_id)?;
        self.ctrl_command(
            container_id,
            &format!("refresh-client -A '{}:on'", pane_id.replace('\'', "'\\''")),
        )?;
        let initial_screen = self.capture_pane(container_id, pane_id);
        let writer = self.pane_writer(container_id, pane_id)?;

        // Force resize to trigger repaint.
        self.force_resize(container_id, pane_id, rows, cols)?;

        Ok(AdoptedSession {
            output: Box::new(reader),
            input: Box::new(writer),
            initial_screen,
        })
    }

    /// Force a resize to trigger SIGWINCH in the container's pane.
    fn force_resize(&self, container_id: &str, pane_id: &str, rows: u16, cols: u16) -> Result<()> {
        if rows > 1 {
            self.do_resize(container_id, pane_id, rows - 1, cols)?;
        } else {
            self.do_resize(container_id, pane_id, rows + 1, cols)?;
        }
        self.do_resize(container_id, pane_id, rows, cols)?;
        Ok(())
    }

    /// Resize a pane inside a container's tmux.
    fn do_resize(&self, container_id: &str, pane_id: &str, rows: u16, cols: u16) -> Result<()> {
        self.ctrl_command(
            container_id,
            &format!("resize-window -t {pane_id} -x {cols} -y {rows}"),
        )?;
        self.ctrl_command(
            container_id,
            &format!("resize-pane -t {pane_id} -x {cols} -y {rows}"),
        )?;
        Ok(())
    }

    /// Build a shell command string (same logic as other backends).
    fn build_shell_command(command: &str, args: &[String]) -> String {
        let mut parts = vec![command.to_string()];
        for arg in args {
            parts.push(control_mode::shell_escape(arg));
        }
        parts.join(" ")
    }

    /// Disconnect from a container's control mode.
    pub fn disconnect_container(&self, container_id: &str) {
        if let Ok(mut controls) = self.controls.lock() {
            controls.remove(container_id);
        }
    }
}

impl SessionBackend for DevcontainerBackend {
    fn name(&self) -> &str {
        "devcontainer"
    }

    fn check_available(&self) -> Result<()> {
        ContainerManager::check_available(self.runtime)
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
        // The target container ID is passed via the env map (injected by Session::spawn).
        let container_id = env
            .get(crate::agent::backend::CONTAINER_ID_ENV_KEY)
            .cloned()
            .context("No container ID in env — caller must set SessionConfig.container_id")?;

        let shell_cmd = Self::build_shell_command(command, args);

        let cwd_part = match cwd {
            Some(dir) => {
                format!(" -c {}", control_mode::shell_escape(&dir.to_string_lossy()))
            }
            None => String::new(),
        };
        let cmd = format!(
            "new-window -t {DC_TMUX_SESSION} -n {window_name} -P -F '#{{pane_id}}'{cwd_part} {shell_cmd}"
        );
        let result = self.ctrl_command(&container_id, &cmd)?;
        let pane_id = result.trim().to_string();

        debug!(container_id = %container_id, pane_id = %pane_id, "Container tmux window created");

        let connected = self.connect_pane(&container_id, &pane_id, rows, cols)?;
        let composite_id = Self::make_backend_id(&container_id, &pane_id);

        Ok(SpawnedSession {
            backend_id: composite_id,
            output: connected.output,
            input: connected.input,
            initial_screen: connected.initial_screen,
        })
    }

    fn adopt(&self, backend_id: &str, rows: u16, cols: u16) -> Result<AdoptedSession> {
        let (container_id, pane_id) = Self::parse_backend_id(backend_id)?;
        self.ensure_control(container_id)?;
        self.connect_pane(container_id, pane_id, rows, cols)
    }

    fn discover(&self) -> Result<Vec<DiscoveredSession>> {
        let controls = self
            .controls
            .lock()
            .map_err(|e| anyhow::anyhow!("controls lock: {e}"))?;

        let mut sessions = Vec::new();
        for (container_id, ctrl) in controls.iter() {
            let result = match ctrl.send_command(&format!(
                "list-windows -t {DC_TMUX_SESSION} -F '#{{pane_id}}|#{{window_name}}|#{{pane_dead}}'"
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
                    backend_id: Self::make_backend_id(container_id, parts[0]),
                    name: window_name.to_string(),
                    is_alive: parts[2] != "1",
                });
            }
        }

        Ok(sessions)
    }

    fn resize(&self, backend_id: &str, rows: u16, cols: u16) -> Result<()> {
        let (container_id, pane_id) = Self::parse_backend_id(backend_id)?;
        self.do_resize(container_id, pane_id, rows, cols)
    }

    fn is_dead(&self, backend_id: &str) -> Result<bool> {
        let (container_id, pane_id) = Self::parse_backend_id(backend_id)?;
        let result = self.ctrl_command(
            container_id,
            &format!("display-message -t {pane_id} -p '#{{pane_dead}}'"),
        )?;
        Ok(result.trim() == "1")
    }

    fn kill(&self, backend_id: &str) -> Result<()> {
        let (container_id, pane_id) = Self::parse_backend_id(backend_id)?;
        let _ = self.unregister_pane(container_id, pane_id);
        self.ctrl_command(container_id, &format!("kill-pane -t {pane_id}"))?;
        Ok(())
    }

    fn detach(&self, backend_id: &str) -> Result<()> {
        let (container_id, pane_id) = Self::parse_backend_id(backend_id)?;
        if let Err(e) = self.ctrl_command_nowait(
            container_id,
            &format!("refresh-client -A '{}:off'", pane_id.replace('\'', "'\\''")),
        ) {
            warn!("Failed to disable container output monitoring during detach: {e}");
        }
        let _ = self.unregister_pane(container_id, pane_id);
        Ok(())
    }

    fn prepare_vm(&self, container_id: &str) -> Result<()> {
        self.ensure_control(container_id)
    }

    fn default_shell(&self) -> String {
        "/bin/bash".to_string()
    }

    fn pane_pid(&self, backend_id: &str) -> Result<Option<u32>> {
        let (container_id, pane_id) = Self::parse_backend_id(backend_id)?;
        let result = self.ctrl_command(
            container_id,
            &format!("display-message -t {pane_id} -p '#{{pane_pid}}'"),
        )?;
        Ok(result.trim().parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_to_container_path_basic() {
        let result = host_to_container_path(Path::new("/home/user/projects/my-app"));
        assert_eq!(result, PathBuf::from("/workspaces/my-app"));
    }

    #[test]
    fn host_to_container_path_trailing_slash() {
        let result = host_to_container_path(Path::new("/home/user/projects/my-app/"));
        assert_eq!(result, PathBuf::from("/workspaces/my-app"));
    }

    #[test]
    fn host_to_container_path_root_fallback() {
        let result = host_to_container_path(Path::new("/"));
        assert_eq!(result, PathBuf::from("/workspaces/workspace"));
    }

    #[test]
    fn parse_backend_id_valid() {
        let (cid, pid) = DevcontainerBackend::parse_backend_id("dc:abc-123:%42").unwrap();
        assert_eq!(cid, "abc-123");
        assert_eq!(pid, "%42");
    }

    #[test]
    fn parse_backend_id_invalid_prefix() {
        assert!(DevcontainerBackend::parse_backend_id("vm:abc:%42").is_err());
    }

    #[test]
    fn parse_backend_id_too_few_parts() {
        assert!(DevcontainerBackend::parse_backend_id("dc:abc").is_err());
    }

    #[test]
    fn make_backend_id_format() {
        assert_eq!(
            DevcontainerBackend::make_backend_id("abc-123", "%42"),
            "dc:abc-123:%42"
        );
    }

    #[test]
    fn container_runtime_cmd() {
        assert_eq!(ContainerRuntime::Podman.cmd(), "podman");
        assert_eq!(ContainerRuntime::Docker.cmd(), "docker");
    }

    #[test]
    fn container_runtime_display() {
        assert_eq!(ContainerRuntime::Podman.to_string(), "podman");
        assert_eq!(ContainerRuntime::Docker.to_string(), "docker");
    }

    #[test]
    fn container_state_db_roundtrip() {
        let states = vec![
            ContainerState::Building,
            ContainerState::Starting,
            ContainerState::Ready,
            ContainerState::Stopping,
            ContainerState::Stopped,
            ContainerState::Failed("build error".to_string()),
        ];
        for state in states {
            let db_str = state.to_db_str();
            let restored = ContainerState::from_db_str(&db_str);
            assert_eq!(state, restored);
        }
    }

    #[test]
    fn container_state_unknown_defaults_to_stopped() {
        assert_eq!(
            ContainerState::from_db_str("unknown"),
            ContainerState::Stopped
        );
    }

    #[test]
    fn container_config_default() {
        let config = ContainerConfig::default();
        assert!(config.image.is_none());
        assert_eq!(config.cpus, 2);
        assert_eq!(config.memory_mb, 2048);
        assert!(config.firewall_enabled);
        assert_eq!(config.containerfile, Some("default".to_string()));
    }

    #[test]
    fn make_then_parse_backend_id_roundtrip() {
        let id = DevcontainerBackend::make_backend_id("my-container-uuid", "%99");
        let (cid, pid) = DevcontainerBackend::parse_backend_id(&id).unwrap();
        assert_eq!(cid, "my-container-uuid");
        assert_eq!(pid, "%99");
    }

    #[test]
    fn parse_backend_id_preserves_colons_in_pane_id() {
        // splitn(3, ':') means the third part captures everything after the second ':'
        let (cid, pid) = DevcontainerBackend::parse_backend_id("dc:abc:%42:extra").unwrap();
        assert_eq!(cid, "abc");
        assert_eq!(pid, "%42:extra");
    }

    #[test]
    fn build_shell_command_no_args() {
        let cmd = DevcontainerBackend::build_shell_command("claude", &[]);
        assert_eq!(cmd, "claude");
    }

    #[test]
    fn build_shell_command_with_args() {
        let cmd = DevcontainerBackend::build_shell_command(
            "claude",
            &["--resume".to_string(), "session-id".to_string()],
        );
        assert!(cmd.starts_with("claude "));
        assert!(cmd.contains("--resume"));
        assert!(cmd.contains("session-id"));
    }

    #[test]
    fn container_state_display() {
        assert_eq!(ContainerState::Building.to_string(), "Building");
        assert_eq!(ContainerState::Ready.to_string(), "Ready");
        assert_eq!(
            ContainerState::Failed("oops".to_string()).to_string(),
            "Failed: oops"
        );
    }

    #[test]
    fn default_containerfile_has_thurbox_user() {
        assert!(DEFAULT_CONTAINERFILE.contains("useradd"));
        assert!(DEFAULT_CONTAINERFILE.contains("thurbox"));
        assert!(DEFAULT_CONTAINERFILE.contains("-u 5000"));
        assert!(DEFAULT_CONTAINERFILE.contains("-g 5000"));
    }

    #[test]
    fn default_containerfile_has_rsync() {
        assert!(DEFAULT_CONTAINERFILE.contains("rsync"));
    }

    #[test]
    fn default_containerfile_has_path_env() {
        assert!(DEFAULT_CONTAINERFILE.contains("ENV PATH=\"/home/thurbox/.local/bin:${PATH}\""));
    }

    #[test]
    fn default_containerfile_uses_native_installer() {
        assert!(DEFAULT_CONTAINERFILE.contains("claude.ai/install.sh"));
        assert!(!DEFAULT_CONTAINERFILE.contains("npm install"));
    }

    #[test]
    fn default_allowlist_is_nonempty() {
        assert!(!DEFAULT_ALLOWLIST.trim().is_empty());
        assert!(DEFAULT_ALLOWLIST.contains("api.anthropic.com"));
    }

    #[test]
    fn start_poll_constants_are_sensible() {
        let max_attempts = CONTAINER_START_MAX_ATTEMPTS;
        let poll_ms = CONTAINER_START_POLL_INTERVAL.as_millis();
        assert!(max_attempts >= 5, "Too few attempts: {max_attempts}");
        assert!(poll_ms >= 100, "Poll interval too short: {poll_ms}ms");
        assert!(poll_ms <= 2000, "Poll interval too long: {poll_ms}ms");
    }
}
