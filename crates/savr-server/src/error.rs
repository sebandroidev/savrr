use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// The API error envelope from PRD-04 §1:
/// `{ "error": { "code", "message", "detail" } }` + a matching HTTP status.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub detail: Value,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            detail: json!({}),
        }
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
    }

    pub fn unauthorized(m: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", m)
    }

    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", m)
    }

    pub fn bad_request(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", m)
    }

    pub fn forbidden(m: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", m)
    }

    /// 429 — pairing rate limit / lockout (PRD-06 §6).
    pub fn too_many(m: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", m)
    }

    /// 409 for a stale `PUT /config` (optimistic concurrency, PRD-04 §2).
    /// Carries the server's current tag so the client can rebase and retry.
    pub fn config_conflict(current_tag: &str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "config_conflict",
            message: "config tag is stale; refetch /config and retry".into(),
            detail: json!({ "current_tag": current_tag }),
        }
    }

    pub fn blob_missing(hash: &str) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "blob_missing",
            message: format!("blob {hash} not present; PUT it before creating the version"),
            detail: json!({ "blob_hash": hash }),
        }
    }

    /// 409 with both branch tips (PRD-03 §4). `head` is null only if the game
    /// had no head yet but the client still claimed a parent.
    pub fn conflict(head: Value, incoming: Value) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: "version parent is not the current head".into(),
            detail: json!({ "head": head, "incoming": incoming }),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": { "code": self.code, "message": self.message, "detail": self.detail }
        });
        (self.status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::internal(e)
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::internal(e)
    }
}
