//! Thurbox MCP server binary.
//!
//! Exposes Thurbox project, role, and session management over the
//! Model Context Protocol (MCP) via stdio or Streamable HTTP transport.

use std::sync::Arc;

use clap::Parser;
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServiceExt;
use tokio_util::sync::CancellationToken;

/// Transport mode for the MCP server.
#[derive(Clone, Debug, clap::ValueEnum)]
enum Transport {
    /// Stdio transport (default) — reads JSON-RPC from stdin, writes to stdout.
    Stdio,
    /// Streamable HTTP transport — serves MCP over HTTP POST with optional SSE streaming.
    StreamableHttp,
}

/// Thurbox MCP server — manage projects, roles, and sessions.
#[derive(Parser, Debug)]
#[command(name = "thurbox-mcp")]
struct Cli {
    /// Transport to use.
    #[arg(long, default_value = "stdio", value_enum)]
    transport: Transport,

    /// Host address to bind (streamable-http only).
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to bind (streamable-http only).
    #[arg(long, default_value_t = 8080)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Logging to stderr (stdout is owned by the MCP protocol in stdio mode).
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

    match cli.transport {
        Transport::Stdio => {
            let server = thurbox::mcp::ThurboxMcp::new(&db_path)?;
            let service = server.serve(stdio()).await.map_err(|e| {
                tracing::error!("MCP server error: {e}");
                e
            })?;
            service.waiting().await?;
        }
        Transport::StreamableHttp => {
            let ct = CancellationToken::new();

            let service = StreamableHttpService::new(
                move || thurbox::mcp::ThurboxMcp::new(&db_path).map_err(std::io::Error::other),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig {
                    cancellation_token: ct.child_token(),
                    ..Default::default()
                },
            );

            let router = axum::Router::new().nest_service("/mcp", service);
            let addr = format!("{}:{}", cli.host, cli.port);
            let listener = tokio::net::TcpListener::bind(&addr).await?;

            tracing::info!("Streamable HTTP MCP server listening on {addr}/mcp");

            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    tokio::signal::ctrl_c()
                        .await
                        .expect("failed to listen for ctrl+c");
                    tracing::info!("Shutting down...");
                    ct.cancel();
                })
                .await?;
        }
    }

    Ok(())
}
