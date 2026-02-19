/// Integration test for the MCP Streamable HTTP transport.
///
/// Starts the server on a random port and verifies it responds
/// to an MCP initialize request over HTTP POST.
use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

use thurbox::mcp::ThurboxMcp;

fn temp_db_path() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[tokio::test]
async fn streamable_http_initialize() {
    let tmp = temp_db_path();
    let db_path = tmp.path().join("test.db");

    // Ensure the DB is valid by creating and dropping a ThurboxMcp.
    ThurboxMcp::new(&db_path).unwrap();

    let ct = CancellationToken::new();

    let service = StreamableHttpService::new(
        {
            let db_path = db_path.clone();
            move || ThurboxMcp::new(&db_path).map_err(std::io::Error::other)
        },
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig {
            cancellation_token: ct.child_token(),
            ..Default::default()
        },
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn({
        let ct = ct.clone();
        async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct.cancelled().await })
                .await
                .unwrap();
        }
    });

    // Send an MCP initialize request.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0.1.0" }
                },
                "id": 1
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    // The response should be SSE containing the initialize result.
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"result\""),
        "Expected initialize result in SSE body: {body}"
    );
    assert!(
        body.contains("serverInfo"),
        "Expected serverInfo in response: {body}"
    );

    ct.cancel();
}
