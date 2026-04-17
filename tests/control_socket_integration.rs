//! End-to-end sanity check: stand up a real [`control_socket`] listener
//! against an empty `PluginRuntime`, then drive it with the real
//! [`ControlClient`]. Catches regressions where the wire constants drift
//! between the listener and the client despite the two sharing `plugin_bridge::wire`.

use std::sync::Arc;

use thurbox::plugin::control_socket;
use thurbox::plugin::PluginRuntime;
use thurbox::plugin_bridge::{ControlClient, ControlError};
use thurbox::storage::Database;

#[tokio::test(flavor = "multi_thread")]
async fn client_and_listener_round_trip_plugin_list() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("control.sock");
    let db = Database::open(&tmp.path().join("t.db")).unwrap();
    let runtime = Arc::new(PluginRuntime::new());

    let _handle = control_socket::spawn(Arc::clone(&runtime), db, socket_path.clone())
        .await
        .expect("listener should bind");

    let client = ControlClient::new(socket_path);
    let plugins = client.list_plugins().await.expect("empty list");
    assert!(plugins.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn client_surfaces_server_error_for_unknown_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("control.sock");
    let db = Database::open(&tmp.path().join("t.db")).unwrap();
    let runtime = Arc::new(PluginRuntime::new());

    let _handle = control_socket::spawn(Arc::clone(&runtime), db, socket_path.clone())
        .await
        .unwrap();

    let client = ControlClient::new(socket_path);
    let err = client.list_plugin_tools("ghost").await.unwrap_err();
    match err {
        ControlError::Server(msg) => {
            assert!(
                msg.contains("ghost") || msg.contains("unknown plugin"),
                "got: {msg}"
            )
        }
        other => panic!("expected Server error, got {other:?}"),
    }
}
