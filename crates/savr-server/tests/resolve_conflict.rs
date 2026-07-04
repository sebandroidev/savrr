//! Conflict resolution (PRD-03 §4): a stale-parent backup diverges, the owner
//! resolves it by choosing the divergent branch as winner, the head moves, and
//! the loser (previous head) is preserved as a branch — nothing is lost.

mod common;

use axum::body::Body;
use axum::http::StatusCode;
use common::{send, setup};
use serde_json::json;

#[tokio::test]
async fn resolve_moves_head_and_preserves_loser() {
    let ctx = setup().await;
    let app = ctx.app.clone();
    let token = ctx.token.clone();
    let device = ctx.device;

    // Game + blob.
    let (_, game) = send(
        &app,
        "POST",
        "/api/v1/games",
        &token,
        Body::from(json!({ "title": "Conflicted", "steam_appid": null }).to_string()),
        true,
    )
    .await;
    let game_id = game["id"].as_str().unwrap().to_string();

    let archive = b"conflict archive";
    let hash = blake3::hash(archive).to_hex().to_string();
    send(
        &app,
        "PUT",
        &format!("/api/v1/blobs/{hash}"),
        &token,
        Body::from(archive.to_vec()),
        false,
    )
    .await;

    let make_version = |parent: Option<&str>| {
        json!({
            "parent": parent,
            "kind": "full",
            "files": [],
            "blob_hash": hash,
            "bytes": archive.len(),
            "device_id": device.to_string(),
        })
        .to_string()
    };

    // v1: fast-forward, head = v1.
    let (st, v1) = send(
        &app,
        "POST",
        &format!("/api/v1/games/{game_id}/versions"),
        &token,
        Body::from(make_version(None)),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let v1_id = v1["id"].as_str().unwrap().to_string();

    // v2 from stale parent -> 409, stored as a divergent branch.
    let (st, err) = send(
        &app,
        "POST",
        &format!("/api/v1/games/{game_id}/versions"),
        &token,
        Body::from(make_version(None)),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let v2_id = err["error"]["detail"]["incoming"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(v1_id, v2_id);

    // Resolve: winner = v2 (the divergent branch), keep_both.
    let (st, head) = send(
        &app,
        "POST",
        &format!("/api/v1/games/{game_id}/resolve"),
        &token,
        Body::from(json!({ "winner": v2_id, "keep_both": true }).to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "resolve must succeed");
    assert_eq!(
        head["head"].as_str().unwrap(),
        v2_id,
        "head moves to the winner"
    );

    // GET /head confirms the move persisted.
    let (_, head) = send(
        &app,
        "GET",
        &format!("/api/v1/games/{game_id}/head"),
        &token,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(head["head"].as_str().unwrap(), v2_id);

    // The loser (v1) is still present — preserved as a branch, never deleted.
    let (_, versions) = send(
        &app,
        "GET",
        &format!("/api/v1/games/{game_id}/versions"),
        &token,
        Body::empty(),
        false,
    )
    .await;
    let ids: Vec<&str> = versions
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&v1_id.as_str()), "loser is preserved");
    assert!(ids.contains(&v2_id.as_str()));

    // The resolution + keep-both redirect was recorded (PRD-03 §4).
    let (loser, keep_both, redirect): (String, i64, Option<String>) = sqlx::query_as(
        "SELECT loser, keep_both, redirect FROM resolved_conflicts WHERE game_id = ?",
    )
    .bind(&game_id)
    .fetch_one(&ctx.pool)
    .await
    .unwrap();
    assert_eq!(loser, v1_id);
    assert_eq!(keep_both, 1);
    assert!(
        redirect.is_some(),
        "keep_both records a sibling-folder redirect"
    );
}
