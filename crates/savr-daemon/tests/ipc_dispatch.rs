//! Drives the IPC request handler over an in-memory duplex (no real socket),
//! proving `serve_connection` decodes `GuiRequest` frames, dispatches them to
//! the engine, and encodes `DaemonMsg` replies (PRD-05 §4).

use std::sync::Arc;

use savr_core::ipc::{read_frame, write_frame, DaemonMsg, GuiRequest, RootKind, RootSpec};
use savr_daemon::config::DaemonConfig;
use savr_daemon::engine::Engine;
use savr_daemon::ipc::serve_connection;
use savr_daemon::secrets::{FileStore, SecretStore};
use savr_daemon::state::LocalState;
use tokio::sync::broadcast;

async fn test_engine() -> Arc<Engine> {
    let state = LocalState::open_memory().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    // File-backed secret store (empty) so the test never touches the keychain.
    let secret_store: Arc<dyn SecretStore> =
        Arc::new(FileStore::new(dir.path().join("creds.json")));
    // Keep the tempdir alive for the test's lifetime.
    std::mem::forget(dir);
    let (events, _rx) = broadcast::channel(16);
    Engine::new(DaemonConfig::default(), state, secret_store, events)
        .await
        .unwrap()
}

/// Read the next reply, skipping any pushed detection events. Every connection
/// carries the live event stream (e.g. AddRoot triggers a catalog refresh whose
/// `CatalogUpdated` event interleaves), so the real GUI client skips them too.
async fn read_reply<R>(r: &mut R) -> DaemonMsg
where
    R: tokio::io::AsyncRead + Unpin,
{
    loop {
        match read_frame::<_, DaemonMsg>(r).await.unwrap().unwrap() {
            DaemonMsg::Event(_) => continue,
            msg => return msg,
        }
    }
}

#[tokio::test]
async fn dispatches_requests_over_duplex() {
    let engine = test_engine().await;
    let (mut client, server) = tokio::io::duplex(64 * 1024);

    let handle = tokio::spawn(async move {
        serve_connection(engine, server).await.unwrap();
    });

    // GetStatus → Status (a newtype-of-struct variant, encodes fine).
    write_frame(&mut client, &GuiRequest::GetStatus)
        .await
        .unwrap();
    match read_reply(&mut client).await {
        DaemonMsg::Status(s) => {
            assert_eq!(s.version, env!("CARGO_PKG_VERSION"));
            assert_eq!(s.watched_games, 0);
        }
        other => panic!("expected Status, got {other:?}"),
    }

    // GetConfig → Config (newtype-of-struct, encodes fine).
    write_frame(&mut client, &GuiRequest::GetConfig)
        .await
        .unwrap();
    match read_reply(&mut client).await {
        DaemonMsg::Config(_) => {}
        other => panic!("expected Config, got {other:?}"),
    }

    // AddRoot → Ok (a newtype-of-struct request; unit reply).
    write_frame(
        &mut client,
        &GuiRequest::AddRoot(RootSpec {
            kind: RootKind::Drive,
            path: "/tmp/games".to_string(),
        }),
    )
    .await
    .unwrap();
    let msg = read_reply(&mut client).await;
    assert!(matches!(msg, DaemonMsg::Ok), "expected Ok, got {msg:?}");

    // ListRoots exercises dispatch, but its reply type `DaemonMsg::Roots` is a
    // newtype-of-Vec variant that the frozen core's `#[serde(tag = "type")]`
    // cannot encode (see integration notes). The connection must survive and
    // degrade to an Error frame; a future core fix would make it a `Roots`.
    write_frame(&mut client, &GuiRequest::ListRoots)
        .await
        .unwrap();
    match read_reply(&mut client).await {
        DaemonMsg::Error { message } => {
            assert!(message.contains("encode"), "unexpected error: {message}");
        }
        DaemonMsg::Roots(r) => assert_eq!(r.len(), 1), // if core is ever fixed
        other => panic!("expected Roots/Error, got {other:?}"),
    }

    // Unknown game backup → Error (graceful, not a panic).
    write_frame(
        &mut client,
        &GuiRequest::BackupNow {
            game_id: uuid::Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let msg = read_reply(&mut client).await;
    assert!(matches!(msg, DaemonMsg::Error { .. }), "got {msg:?}");

    // Closing the client cleanly ends the server task.
    drop(client);
    handle.await.unwrap();
}
