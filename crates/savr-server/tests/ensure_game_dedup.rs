//! ensure_game dedups Steam games by their stable appid, not their mutable
//! title. Two devices can send different titles for the same appid (Ludusavi
//! manifest title vs .acf name, localization); they must map to ONE game so
//! history isn't split. Custom games (no appid) still dedup by title.

mod common;

use axum::body::Body;
use axum::http::StatusCode;
use common::{send, setup};
use serde_json::json;

async fn ensure(app: &axum::Router, token: &str, body: serde_json::Value) -> String {
    let (st, game) = send(
        app,
        "POST",
        "/api/v1/games",
        token,
        Body::from(body.to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "ensure_game must succeed: {game}");
    game["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn dedups_by_appid_not_title() {
    let ctx = setup().await;
    let app = ctx.app.clone();

    let id1 = ensure(&app, &ctx.token, json!({ "title": "Half-Life 2", "steam_appid": 220 })).await;
    // Same appid, different title (as another device would send) -> same game.
    let id2 = ensure(
        &app,
        &ctx.token,
        json!({ "title": "Half-Life 2 (Steam)", "steam_appid": 220 }),
    )
    .await;
    assert_eq!(id2, id1, "same appid must dedup to one game regardless of title");

    // A different appid is a different game.
    let id3 = ensure(&app, &ctx.token, json!({ "title": "Portal", "steam_appid": 400 })).await;
    assert_ne!(id3, id1, "different appid must be a distinct game");

    // Custom games (no appid) still dedup by title.
    let c1 = ensure(&app, &ctx.token, json!({ "title": "My Mod", "steam_appid": null })).await;
    let c2 = ensure(&app, &ctx.token, json!({ "title": "My Mod", "steam_appid": null })).await;
    assert_eq!(c2, c1, "custom games dedup by title");
}
