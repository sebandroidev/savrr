//! WebSocket catch-up on reconnect (PRD-04 §4 / PRD-03 §5): a device that
//! connects and sends `Hello` with a stale `last_seq` is replayed the
//! `version_available` events it missed. Also checks `ping -> pong`.
//!
//! This drives a real socket (tokio-tungstenite) against an `axum::serve`d
//! router bound to an ephemeral port — the full upgrade + auth + hub path.

mod common;

use std::time::Duration;

use common::TEST_SECRET;
use futures_util::{SinkExt, StreamExt};
use savr_server::{build_app, connect, migrate, AppState, FsBlobStore};
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::{ClientRequestBuilder, Message};
use uuid::Uuid;

#[tokio::test]
async fn hello_replays_missed_version() {
    let pool = connect("sqlite::memory:").await.unwrap();
    migrate(&pool).await.unwrap();

    let account = Uuid::now_v7();
    let device = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO accounts (id, owner_hash, created_at) VALUES (?, 'x', '2026-01-01T00:00:00Z')",
    )
    .bind(account.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO devices (id, account_id, name, os, token_hash, last_seen, revoked)
         VALUES (?, ?, 'Deck', 'linux', 'x', NULL, 0)",
    )
    .bind(device.to_string())
    .bind(account.to_string())
    .execute(&pool)
    .await
    .unwrap();

    // One game + one version to be replayed.
    let game = Uuid::now_v7();
    sqlx::query("INSERT INTO games (id, account_id, title) VALUES (?, ?, 'WS Game')")
        .bind(game.to_string())
        .bind(account.to_string())
        .execute(&pool)
        .await
        .unwrap();
    let vid = Uuid::now_v7();
    let hash = blake3::hash(b"ws").to_hex().to_string();
    sqlx::query("INSERT INTO blobs (hash, bytes, refcount, created_at) VALUES (?, 2, 1, '2026-01-01T00:00:00Z')")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO versions
         (id, game_id, account_id, device_id, parent, kind, blob_hash, files_json, bytes, seq, created_at)
         VALUES (?, ?, ?, ?, NULL, 'full', ?, '[]', 2, 1, '2026-01-01T00:00:00Z')",
    )
    .bind(vid.to_string())
    .bind(game.to_string())
    .bind(account.to_string())
    .bind(device.to_string())
    .bind(&hash)
    .execute(&pool)
    .await
    .unwrap();

    // Serve the router on an ephemeral port.
    let dir = std::env::temp_dir().join(format!("savr-ws-{}", Uuid::now_v7()));
    let state = AppState::new(pool.clone(), FsBlobStore::new(dir), TEST_SECRET.to_vec());
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Connect with a real access token on the upgrade request. The builder
    // generates the RFC 6455 handshake headers and keeps our Authorization.
    let token = savr_server::auth::mint_access_token(TEST_SECRET, account, device).unwrap();
    let uri: Uri = format!("ws://{addr}/api/v1/ws").parse().unwrap();
    let req =
        ClientRequestBuilder::new(uri).with_header("Authorization", format!("Bearer {token}"));
    let (mut ws, _resp) = tokio_tungstenite::connect_async(req).await.unwrap();

    // Hello with empty last_seq -> everything (seq > 0) should replay.
    let hello = serde_json::json!({
        "type": "hello",
        "device_id": device.to_string(),
        "last_seq": {}
    })
    .to_string();
    ws.send(Message::Text(hello)).await.unwrap();

    let replayed = recv_typed(&mut ws, "version_available").await;
    assert_eq!(replayed["version_id"].as_str().unwrap(), vid.to_string());
    assert_eq!(replayed["game_id"].as_str().unwrap(), game.to_string());
    assert_eq!(replayed["seq"].as_i64().unwrap(), 1);

    // ping -> pong.
    ws.send(Message::Text(
        serde_json::json!({ "type": "ping" }).to_string(),
    ))
    .await
    .unwrap();
    let pong = recv_typed(&mut ws, "pong").await;
    assert_eq!(pong["type"], "pong");
}

/// Read text frames until one with `{"type": <want>}` arrives (skips control
/// frames), failing on timeout or early close.
async fn recv_typed<S>(ws: &mut S, want: &str) -> serde_json::Value
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == want {
                        return v;
                    }
                }
                Some(Ok(_)) => continue,
                other => panic!("socket closed before {want}: {other:?}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(5), fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {want}"))
}
