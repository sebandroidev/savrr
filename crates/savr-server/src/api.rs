//! HTTP surface: the router, the JWT auth extractors, and the handlers.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRef, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, head, post};
use axum::{Json, Router};
use savr_core::protocol::{
    EnsureGameRequest, LoginRequest, LoginResponse, PairRequest, PairResponse, PairingCodeResponse,
    RefreshRequest, ResolveRequest, ServerMsg, TokenResponse,
};
use savr_core::{CreateVersion, SyncedConfig};
use serde_json::json;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::auth;
use crate::blobs::FsBlobStore;
use crate::db::{self, Advance};
use crate::error::ApiError;
use crate::hub::Hub;

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::SqlitePool,
    pub blobs: FsBlobStore,
    pub hub: Hub,
    /// HS256 signing secret for access + session JWTs (PRD-06 §3).
    pub jwt_secret: Arc<Vec<u8>>,
    /// Pairing rate-limit / lockout state.
    pub pair_guard: auth::PairGuard,
}

impl AppState {
    pub fn new(pool: sqlx::SqlitePool, blobs: FsBlobStore, jwt_secret: Vec<u8>) -> Self {
        Self {
            pool,
            blobs,
            hub: Hub::new(),
            jwt_secret: Arc::new(jwt_secret),
            pair_guard: auth::PairGuard::new(),
        }
    }
}

/// An authenticated device: the account + device the access JWT belongs to
/// (PRD-06 §3). Every data endpoint scopes its queries by `account_id`.
pub struct Authed {
    pub account_id: Uuid,
    pub device_id: Uuid,
}

/// An authenticated owner session — only mints pairing codes.
pub struct OwnerSession {
    pub account_id: Uuid,
}

fn bearer(parts: &Parts) -> Result<String, ApiError> {
    let value = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("missing Authorization header"))?;
    value
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_string())
        .ok_or_else(|| ApiError::unauthorized("expected Bearer token"))
}

impl<S> FromRequestParts<S> for Authed
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app = AppState::from_ref(state);
        let token = bearer(parts)?;
        let claims = auth::verify_access(&app.jwt_secret, &token)?;
        let account = Uuid::parse_str(&claims.acc)
            .map_err(|_| ApiError::unauthorized("malformed acc claim"))?;
        let device = Uuid::parse_str(&claims.sub)
            .map_err(|_| ApiError::unauthorized("malformed sub claim"))?;
        // Revocation must bite promptly, not only when the JWT expires (PRD-06 §3).
        if !db::device_active(&app.pool, account, device).await? {
            return Err(ApiError::unauthorized("device revoked or unknown"));
        }
        Ok(Authed {
            account_id: account,
            device_id: device,
        })
    }
}

impl<S> FromRequestParts<S> for OwnerSession
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app = AppState::from_ref(state);
        let token = bearer(parts)?;
        let claims = auth::verify_owner_session(&app.jwt_secret, &token)?;
        let account = Uuid::parse_str(&claims.acc)
            .map_err(|_| ApiError::unauthorized("malformed acc claim"))?;
        Ok(OwnerSession {
            account_id: account,
        })
    }
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        // Liveness/readiness (PRD-07) — no auth.
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        // Auth & devices (PRD-06).
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/refresh", post(refresh))
        .route("/api/v1/devices/pair-code", post(create_pair_code))
        .route("/api/v1/devices/pair", post(pair))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/{id}", delete(delete_device))
        // Games & versions (PRD-04).
        .route("/api/v1/games", post(ensure_game))
        .route(
            "/api/v1/games/{id}/versions",
            post(create_version).get(list_versions),
        )
        .route("/api/v1/games/{id}/head", get(get_head))
        .route("/api/v1/games/{id}/resolve", post(resolve))
        // Synced config.
        .route("/api/v1/config", get(get_config).put(put_config))
        // Blobs.
        .route(
            "/api/v1/blobs/{hash}",
            head(blob_head).put(blob_put).get(blob_get),
        )
        // WebSocket push.
        .route("/api/v1/ws", get(ws_upgrade))
        .with_state(state)
}

fn valid_hash(hash: &str) -> Result<(), ApiError> {
    if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ApiError::bad_request("blob hash must be 64 hex chars"))
    }
}

// ---- health ----

async fn healthz() -> &'static str {
    "ok"
}

/// Readiness: the DB answers and the blob volume is reachable (PRD-07).
async fn readyz(State(app): State<AppState>) -> Response {
    let db_ok = sqlx::query("SELECT 1").fetch_one(&app.pool).await.is_ok();
    let blob_ok = app.blobs.reachable().await;
    if db_ok && blob_ok {
        (StatusCode::OK, "ready").into_response()
    } else {
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "not ready").into_response()
    }
}

// ---- auth & devices ----

async fn login(
    State(app): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    // Single-owner model: the one seeded account (PRD-06 §1).
    let row = sqlx::query("SELECT id, owner_hash FROM accounts LIMIT 1")
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| ApiError::unauthorized("no owner account provisioned"))?;
    let owner_hash: String = sqlx::Row::get(&row, "owner_hash");
    if !auth::verify_secret(&req.password, &owner_hash) {
        return Err(ApiError::unauthorized("invalid password"));
    }
    let account =
        Uuid::parse_str(&sqlx::Row::get::<String, _>(&row, "id")).map_err(ApiError::internal)?;
    let token = auth::mint_owner_session(&app.jwt_secret, account)?;
    Ok(Json(LoginResponse {
        session_token: token,
        expires_in: auth::SESSION_TTL_SECS,
    })
    .into_response())
}

async fn create_pair_code(
    State(app): State<AppState>,
    owner: OwnerSession,
) -> Result<Response, ApiError> {
    let (code, expires_at) = db::create_pairing_code(&app.pool, owner.account_id).await?;
    Ok(Json(PairingCodeResponse { code, expires_at }).into_response())
}

async fn pair(
    State(app): State<AppState>,
    Json(req): Json<PairRequest>,
) -> Result<Response, ApiError> {
    app.pair_guard.check()?;
    let account = match db::redeem_pairing_code(&app.pool, &req.code).await? {
        Some(acc) => acc,
        None => {
            app.pair_guard.record_fail();
            return Err(ApiError::unauthorized("invalid or expired pairing code"));
        }
    };
    app.pair_guard.record_success();

    let device_id = Uuid::now_v7();
    // Returned once; stored only as an argon2 hash (PRD-06 §3).
    let refresh_secret = auth::generate_refresh_secret();
    let token_hash = auth::hash_secret(&refresh_secret)?;
    db::create_device(
        &app.pool,
        account,
        device_id,
        &req.device_name,
        req.os,
        &token_hash,
    )
    .await?;
    let access_token = auth::mint_access_token(&app.jwt_secret, account, device_id)?;
    app.hub
        .broadcast_account(account, ServerMsg::DeviceAdded { device_id });

    Ok(Json(PairResponse {
        device_id,
        account_id: account,
        refresh_secret,
        access_token,
        expires_in: auth::ACCESS_TTL_SECS,
    })
    .into_response())
}

async fn refresh(
    State(app): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> Result<Response, ApiError> {
    // One generic 401 for all failure modes (no such device / revoked / wrong
    // secret) so the response can't be used as a device-existence or
    // revocation oracle. The internal branches only steer control flow.
    let creds = "invalid device credentials";
    let (account, token_hash, revoked) = db::device_credentials(&app.pool, req.device_id)
        .await?
        .ok_or_else(|| ApiError::unauthorized(creds))?;
    if revoked || !auth::verify_secret(&req.refresh_secret, &token_hash) {
        return Err(ApiError::unauthorized(creds));
    }
    let token = auth::mint_access_token(&app.jwt_secret, account, req.device_id)?;
    Ok(Json(TokenResponse {
        access_token: token,
        expires_in: auth::ACCESS_TTL_SECS,
    })
    .into_response())
}

async fn list_devices(State(app): State<AppState>, auth: Authed) -> Result<Response, ApiError> {
    let devices = db::list_devices(&app.pool, auth.account_id).await?;
    Ok(Json(devices).into_response())
}

async fn delete_device(
    State(app): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !db::revoke_device(&app.pool, auth.account_id, id).await? {
        return Err(ApiError::not_found("device"));
    }
    app.hub.drop_device(auth.account_id, id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---- games ----

async fn ensure_game(
    State(app): State<AppState>,
    auth: Authed,
    Json(req): Json<EnsureGameRequest>,
) -> Result<Response, ApiError> {
    let game = db::ensure_game(&app.pool, auth.account_id, &req.title, req.steam_appid).await?;
    Ok((StatusCode::OK, Json(game)).into_response())
}

async fn get_head(
    State(app): State<AppState>,
    auth: Authed,
    Path(game_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let head = db::get_head(&app.pool, auth.account_id, game_id).await?;
    Ok(Json(head).into_response())
}

async fn list_versions(
    State(app): State<AppState>,
    auth: Authed,
    Path(game_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let versions = db::list_versions(&app.pool, auth.account_id, game_id).await?;
    Ok(Json(versions).into_response())
}

// ---- versions ----

async fn create_version(
    State(app): State<AppState>,
    auth: Authed,
    Path(game_id): Path<Uuid>,
    Json(req): Json<CreateVersion>,
) -> Result<Response, ApiError> {
    // Referential integrity: the blob must be uploaded first (PRD-04 §2).
    let blob_hex = req.blob_hash.to_hex();
    if !app.blobs.exists(&blob_hex).await {
        return Err(ApiError::blob_missing(&blob_hex));
    }
    match db::create_version(&app.pool, auth.account_id, game_id, req).await? {
        Advance::FastForward(v) => {
            app.hub.broadcast_except(
                auth.account_id,
                v.device_id,
                ServerMsg::VersionAvailable {
                    game_id,
                    version_id: v.id,
                    seq: v.seq,
                },
            );
            Ok((StatusCode::CREATED, Json(v)).into_response())
        }
        Advance::Conflict { head, incoming } => {
            let mut tips = Vec::new();
            if let Some(h) = &head {
                tips.push(h.id);
            }
            tips.push(incoming.id);
            app.hub
                .broadcast_account(auth.account_id, ServerMsg::Conflict { game_id, tips });
            let head_json = head.map(|h| json!(h)).unwrap_or(json!(null));
            Err(ApiError::conflict(head_json, json!(incoming)))
        }
    }
}

async fn resolve(
    State(app): State<AppState>,
    auth: Authed,
    Path(game_id): Path<Uuid>,
    Json(req): Json<ResolveRequest>,
) -> Result<Response, ApiError> {
    let (head, seq) = db::resolve_conflict(
        &app.pool,
        auth.account_id,
        game_id,
        req.winner,
        req.keep_both,
    )
    .await?;
    if let Some(version_id) = head.head {
        app.hub.broadcast_except(
            auth.account_id,
            auth.device_id,
            ServerMsg::VersionAvailable {
                game_id,
                version_id,
                seq,
            },
        );
    }
    Ok(Json(head).into_response())
}

// ---- config ----

async fn get_config(State(app): State<AppState>, auth: Authed) -> Result<Response, ApiError> {
    let cfg = db::get_or_seed_config(&app.pool, auth.account_id).await?;
    Ok(Json(cfg).into_response())
}

async fn put_config(
    State(app): State<AppState>,
    auth: Authed,
    Json(cfg): Json<SyncedConfig>,
) -> Result<Response, ApiError> {
    let updated = db::put_config(&app.pool, auth.account_id, cfg).await?;
    app.hub.broadcast_except(
        auth.account_id,
        auth.device_id,
        ServerMsg::ConfigUpdated {
            config_tag: updated.tag.clone(),
        },
    );
    Ok(Json(updated).into_response())
}

// ---- blobs ----

async fn blob_head(
    State(app): State<AppState>,
    _auth: Authed,
    Path(hash): Path<String>,
) -> Result<StatusCode, ApiError> {
    valid_hash(&hash)?;
    // HEAD stays ungated (dedup existence check runs before the version that
    // would reference the blob exists) but still requires a valid device.
    if app.blobs.exists(&hash).await {
        Ok(StatusCode::OK)
    } else {
        Err(ApiError::not_found("blob"))
    }
}

async fn blob_put(
    State(app): State<AppState>,
    _auth: Authed,
    Path(hash): Path<String>,
    body: Body,
) -> Result<Response, ApiError> {
    valid_hash(&hash)?;
    let existed = app.blobs.exists(&hash).await;
    let bytes = app.blobs.put(&hash, body).await?;
    sqlx::query(
        "INSERT OR IGNORE INTO blobs (hash, bytes, refcount, created_at) VALUES (?, ?, 0, ?)",
    )
    .bind(&hash)
    .bind(bytes as i64)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&app.pool)
    .await?;
    let status = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok(status.into_response())
}

async fn blob_get(
    State(app): State<AppState>,
    auth: Authed,
    Path(hash): Path<String>,
) -> Result<Response, ApiError> {
    valid_hash(&hash)?;
    // Blob access gate (PRD-06 §6): only serve a hash this account references,
    // and return 404 (not 403) so a guessed hash reveals nothing.
    if !db::account_references_blob(&app.pool, auth.account_id, &hash).await? {
        return Err(ApiError::not_found("blob"));
    }
    let path = app.blobs.get_path(&hash);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("blob"))?;
    Ok(Body::from_stream(ReaderStream::new(file)).into_response())
}

// ---- websocket ----

async fn ws_upgrade(State(app): State<AppState>, auth: Authed, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| crate::ws::handle_socket(socket, app, auth))
}
