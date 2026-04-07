//! Miscellaneous helper functions used by the app module.

use tracing::info;

pub(super) fn open_url(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok();
}

/// Write a `.mcp.json` file into a VM directory via SSH so Claude Code discovers
/// the project's MCP servers when running inside the sandbox.
pub(super) fn write_vm_mcp_json(
    instance: &crate::agent::vm::VmInstance,
    vm_cwd: &std::path::Path,
    mcp_servers: &[crate::session::McpServerConfig],
) -> anyhow::Result<()> {
    if mcp_servers.is_empty() {
        return Ok(());
    }

    let doc = crate::session::McpServerConfig::to_mcp_json(mcp_servers);
    let json_str = serde_json::to_string_pretty(&doc)?;

    let key_path = instance.ssh_key_path();
    let ssh_port = instance.ssh_port.to_string();
    let user = instance.ssh_user();
    let dest = vm_cwd.join(".mcp.json");

    let shell_cmd = format!("cat > '{}'", dest.to_string_lossy().replace('\'', "'\\''"));
    let output = std::process::Command::new("ssh")
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
            &shell_cmd,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(json_str.as_bytes())?;
            }
            child.wait_with_output()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ssh write .mcp.json failed: {stderr}");
    }

    info!("Wrote .mcp.json into VM at {}", dest.display());
    Ok(())
}

/// Write a `.mcp.json` file into a container directory via `docker exec` so Claude Code
/// discovers the project's MCP servers when running inside the devcontainer.
pub(super) fn write_container_mcp_json(
    runtime: &str,
    docker_container_id: &str,
    container_cwd: &std::path::Path,
    mcp_servers: &[crate::session::McpServerConfig],
) -> anyhow::Result<()> {
    if mcp_servers.is_empty() {
        return Ok(());
    }

    let docker_id = docker_container_id;

    let doc = crate::session::McpServerConfig::to_mcp_json(mcp_servers);
    let json_str = serde_json::to_string_pretty(&doc)?;

    let dest = container_cwd.join(".mcp.json");
    let shell_cmd = format!("cat > '{}'", dest.to_string_lossy().replace('\'', "'\\''"));
    let output = std::process::Command::new(runtime)
        .args(["exec", "-i", docker_id, "sh", "-c", &shell_cmd])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(json_str.as_bytes())?;
            }
            child.wait_with_output()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{runtime} exec write .mcp.json failed: {stderr}");
    }

    info!("Wrote .mcp.json into container at {}", dest.display());
    Ok(())
}
