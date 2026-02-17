//! Thurbox MCP server binary.
//!
//! Exposes Thurbox project, role, and session management over the
//! Model Context Protocol (MCP) via stdio transport.

use rmcp::transport::stdio;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging to stderr (stdout is owned by the MCP protocol).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let db_path = thurbox::paths::database_file()
        .ok_or_else(|| anyhow::anyhow!("Cannot resolve database path (is HOME set?)"))?;

    tracing::info!("Opening database at {}", db_path.display());

    let server = thurbox::mcp::ThurboxMcp::new(&db_path)?;

    let service = server.serve(stdio()).await.map_err(|e| {
        tracing::error!("MCP server error: {e}");
        e
    })?;

    service.waiting().await?;

    Ok(())
}
