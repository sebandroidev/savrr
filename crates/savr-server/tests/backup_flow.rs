//! End-to-end proof for M1: a backup lands server-side, the head advances, and
//! a second device backing up from a stale parent gets a 409 (never silently
//! overwritten — G6 / PRD-03 §4). Now authenticated with a real access JWT.

mod common;

use axum::body::Body;
use axum::http::StatusCode;
use common::{send, setup};
use serde_json::json;

#[tokio::test]
async fn backup_lands_and_conflict_is_reported() {
    let ctx = setup().await;
    let app = ctx.app.clone();
    let token = ctx.token.clone();
    let device = ctx.device;

    // A request without a token is rejected (auth is enforced now).
    let (st, _) = send(&app, "POST", "/api/v1/games", "", Body::empty(), true).await;
    assert_eq!(
        st,
        StatusCode::UNAUTHORIZED,
        "unauthenticated calls must 401"
    );

    // Register a game.
    let (st, game) = send(
        &app,
        "POST",
        "/api/v1/games",
        &token,
        Body::from(json!({ "title": "An Example Game", "steam_appid": 123 }).to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let game_id = game["id"].as_str().unwrap().to_string();

    // Upload a blob (content-addressed by its blake3 hash).
    let archive = b"pretend .savr archive bytes";
    let hash = blake3::hash(archive).to_hex().to_string();
    let (st, _) = send(
        &app,
        "PUT",
        &format!("/api/v1/blobs/{hash}"),
        &token,
        Body::from(archive.to_vec()),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);

    // HEAD confirms dedup existence check works.
    let (st, _) = send(
        &app,
        "HEAD",
        &format!("/api/v1/blobs/{hash}"),
        &token,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let make_version = |parent: Option<&str>| {
        json!({
            "parent": parent,
            "kind": "full",
            "files": [{ "rel_path": "saves/a.sav", "size": 7, "mtime": 0, "hash": hash }],
            "blob_hash": hash,
            "bytes": archive.len(),
            "device_id": device.to_string(),
        })
        .to_string()
    };

    // First backup: parent = null, head was null -> fast-forward (201).
    let (st, v1) = send(
        &app,
        "POST",
        &format!("/api/v1/games/{game_id}/versions"),
        &token,
        Body::from(make_version(None)),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "first backup must fast-forward");
    let v1_id = v1["id"].as_str().unwrap().to_string();

    // Head now points at v1.
    let (st, head) = send(
        &app,
        "GET",
        &format!("/api/v1/games/{game_id}/head"),
        &token,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(head["head"].as_str().unwrap(), v1_id);
    assert_eq!(head["seq"].as_i64().unwrap(), 1);

    // GET /blobs is gated but the account now references the hash -> 200.
    let (st, _) = send(
        &app,
        "GET",
        &format!("/api/v1/blobs/{hash}"),
        &token,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "referencing account may download its blob"
    );

    // Second device backs up from the STALE parent (null again) -> 409 conflict,
    // head unchanged, both tips returned.
    let (st, err) = send(
        &app,
        "POST",
        &format!("/api/v1/games/{game_id}/versions"),
        &token,
        Body::from(make_version(None)),
        true,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "stale parent must conflict, not overwrite"
    );
    assert_eq!(err["error"]["code"], "conflict");
    assert_eq!(
        err["error"]["detail"]["head"]["id"].as_str().unwrap(),
        v1_id
    );
    assert!(err["error"]["detail"]["incoming"]["id"].is_string());

    // Head still v1 — nothing was overwritten.
    let (_, head) = send(
        &app,
        "GET",
        &format!("/api/v1/games/{game_id}/head"),
        &token,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(head["head"].as_str().unwrap(), v1_id);

    // A correct fast-forward from v1 advances the head.
    let (st, _v2) = send(
        &app,
        "POST",
        &format!("/api/v1/games/{game_id}/versions"),
        &token,
        Body::from(make_version(Some(&v1_id))),
        true,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CREATED,
        "backup from current head fast-forwards"
    );

    // History has all three versions (2 on trunk + 1 divergent branch).
    let (st, versions) = send(
        &app,
        "GET",
        &format!("/api/v1/games/{game_id}/versions"),
        &token,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(versions.as_array().unwrap().len(), 3);
}
