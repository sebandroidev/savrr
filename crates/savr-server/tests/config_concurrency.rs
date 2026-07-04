//! Synced config optimistic concurrency (PRD-04 §2): a PUT carrying a stale tag
//! is rejected with 409 and the server's current tag, so a lagging device can
//! never silently clobber a newer config.

mod common;

use axum::body::Body;
use axum::http::StatusCode;
use common::{send, setup};

#[tokio::test]
async fn stale_config_put_is_rejected() {
    let ctx = setup().await;
    let app = ctx.app.clone();
    let token = ctx.token.clone();

    // GET seeds + returns the default config with tag T1.
    let (st, cfg1) = send(&app, "GET", "/api/v1/config", &token, Body::empty(), false).await;
    assert_eq!(st, StatusCode::OK);
    let t1 = cfg1["tag"].as_str().unwrap().to_string();
    assert!(!t1.is_empty());

    // PUT with the matching tag succeeds and rotates the tag to T2.
    let (st, cfg2) = send(
        &app,
        "PUT",
        "/api/v1/config",
        &token,
        Body::from(cfg1.to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "matching tag accepted");
    let t2 = cfg2["tag"].as_str().unwrap().to_string();
    assert_ne!(t1, t2, "successful PUT rotates the tag");

    // PUT again with the now-stale T1 -> 409, exposing the current tag.
    let (st, err) = send(
        &app,
        "PUT",
        "/api/v1/config",
        &token,
        Body::from(cfg1.to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "stale tag must be rejected");
    assert_eq!(err["error"]["code"], "config_conflict");
    assert_eq!(err["error"]["detail"]["current_tag"].as_str().unwrap(), t2);

    // Rebasing onto T2 works again.
    let (st, cfg3) = send(
        &app,
        "PUT",
        "/api/v1/config",
        &token,
        Body::from(cfg2.to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_ne!(cfg3["tag"].as_str().unwrap(), t2);
}
