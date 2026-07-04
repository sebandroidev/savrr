//! Happy-path auth flow (PRD-06): owner login -> mint pairing code -> pair a new
//! device -> use the access token -> refresh it.

mod common;

use axum::body::Body;
use axum::http::StatusCode;
use common::{send, setup, OWNER_PASSWORD};
use serde_json::json;

#[tokio::test]
async fn login_pair_and_refresh() {
    let ctx = setup().await;
    let app = ctx.app.clone();

    // 1. Owner logs in with the seeded password -> session token.
    let (st, login) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        "",
        Body::from(json!({ "password": OWNER_PASSWORD }).to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "owner login must succeed");
    let session = login["session_token"].as_str().unwrap().to_string();

    // Wrong password is rejected.
    let (st, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        "",
        Body::from(json!({ "password": "nope" }).to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // A device access token must NOT be usable as an owner session.
    let (st, _) = send(
        &app,
        "POST",
        "/api/v1/devices/pair-code",
        &ctx.token,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNAUTHORIZED,
        "access token is not an owner session"
    );

    // 2. Owner mints a pairing code.
    let (st, code_resp) = send(
        &app,
        "POST",
        "/api/v1/devices/pair-code",
        &session,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let code = code_resp["code"].as_str().unwrap().to_string();
    assert!(code.len() >= 6 && code.len() <= 8, "code is 6-8 chars");

    // A bad code is rejected.
    let (st, _) = send(
        &app,
        "POST",
        "/api/v1/devices/pair",
        "",
        Body::from(
            json!({ "code": "BADCODE0", "device_name": "Steam Deck", "os": "linux" }).to_string(),
        ),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // 3. New device pairs with the real code.
    let (st, pair) = send(
        &app,
        "POST",
        "/api/v1/devices/pair",
        "",
        Body::from(json!({ "code": code, "device_name": "Steam Deck", "os": "linux" }).to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "valid code pairs a device");
    let access = pair["access_token"].as_str().unwrap().to_string();
    let refresh_secret = pair["refresh_secret"].as_str().unwrap().to_string();
    let device_id = pair["device_id"].as_str().unwrap().to_string();
    assert!(!refresh_secret.is_empty());

    // The same code can't be reused (single-use burn).
    let (st, _) = send(
        &app,
        "POST",
        "/api/v1/devices/pair",
        "",
        Body::from(json!({ "code": code, "device_name": "Dupe", "os": "linux" }).to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "pairing code is single-use");

    // 4. The freshly minted access token works on a protected endpoint, and the
    //    new device shows up in the account's device list.
    let (st, devices) = send(
        &app,
        "GET",
        "/api/v1/devices",
        &access,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let ids: Vec<&str> = devices
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&device_id.as_str()), "paired device is listed");

    // 5. Refresh: exchange the refresh secret for a new access token.
    let (st, tok) = send(
        &app,
        "POST",
        "/api/v1/auth/refresh",
        "",
        Body::from(json!({ "device_id": device_id, "refresh_secret": refresh_secret }).to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "refresh must succeed");
    let refreshed = tok["access_token"].as_str().unwrap().to_string();
    assert!(!refreshed.is_empty());

    // A wrong refresh secret is rejected.
    let (st, _) = send(
        &app,
        "POST",
        "/api/v1/auth/refresh",
        "",
        Body::from(json!({ "device_id": device_id, "refresh_secret": "deadbeef" }).to_string()),
        true,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // 6. Revoke the device -> its access token stops working (revocation bites).
    let (st, _) = send(
        &app,
        "DELETE",
        &format!("/api/v1/devices/{device_id}"),
        &access,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (st, _) = send(
        &app,
        "GET",
        "/api/v1/devices",
        &refreshed,
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "revoked device is locked out");
}
