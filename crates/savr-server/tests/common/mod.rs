//! Shared harness for the integration tests: an in-memory DB + router, a seeded
//! account/device, a real access token, and a `oneshot` request helper.
#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use savr_server::{build_app, connect, migrate, AppState, FsBlobStore};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

/// Fixed HS256 secret so tests can mint tokens the router will accept.
pub const TEST_SECRET: &[u8] = b"savr-integration-test-secret-not-for-prod";
/// The owner password seeded on the test account (for the login/pairing test).
pub const OWNER_PASSWORD: &str = "correct-horse-battery-staple";

pub struct Ctx {
    pub app: axum::Router,
    pub pool: sqlx::SqlitePool,
    pub account: Uuid,
    pub device: Uuid,
    /// A valid device access token for `account`/`device`.
    pub token: String,
}

pub async fn setup() -> Ctx {
    let pool = connect("sqlite::memory:").await.unwrap();
    migrate(&pool).await.unwrap();

    let account = Uuid::now_v7();
    let device = Uuid::now_v7();
    let owner_hash = savr_server::auth::hash_secret(OWNER_PASSWORD).unwrap();
    sqlx::query(
        "INSERT INTO accounts (id, owner_hash, created_at) VALUES (?, ?, '2026-01-01T00:00:00Z')",
    )
    .bind(account.to_string())
    .bind(&owner_hash)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO devices (id, account_id, name, os, token_hash, last_seen, revoked)
         VALUES (?, ?, 'Desktop', 'linux', 'x', NULL, 0)",
    )
    .bind(device.to_string())
    .bind(account.to_string())
    .execute(&pool)
    .await
    .unwrap();

    let dir = std::env::temp_dir().join(format!("savr-blobs-{}", Uuid::now_v7()));
    let state = AppState::new(pool.clone(), FsBlobStore::new(dir), TEST_SECRET.to_vec());
    let app = build_app(state);
    let token = savr_server::auth::mint_access_token(TEST_SECRET, account, device).unwrap();

    Ctx {
        app,
        pool,
        account,
        device,
        token,
    }
}

/// Fire a request at the router. `token` empty => no Authorization header.
pub async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Body,
    json_ct: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if !token.is_empty() {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if json_ct {
        builder = builder.header("content-type", "application/json");
    }
    let resp = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}
