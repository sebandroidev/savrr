//! Auth primitives (PRD-06 §2–3): argon2 hashing of owner password / device
//! refresh secrets, HS256 JWT mint+verify for the two token kinds (device access
//! token, owner session token), pairing-code + secret generation, and the
//! in-process pairing rate-limit / lockout guard.
//!
//! The axum extractors (`Authed`, `OwnerSession`) live in `api.rs`; this module
//! is the pure crypto/token layer so tests and the bootstrap can reuse it.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::password_hash::rand_core::OsRng as ArgonRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ApiError;

/// Access JWT TTL (PRD-06 §3: "short TTL, e.g. 15 min").
pub const ACCESS_TTL_SECS: u64 = 15 * 60;
/// Owner session token TTL — long enough to pair a couple devices in one sitting.
pub const SESSION_TTL_SECS: u64 = 60 * 60;
/// Pairing-code TTL (PRD-06 §2: 5 min).
pub const PAIRING_TTL_SECS: i64 = 5 * 60;

const TYP_ACCESS: &str = "access";
const TYP_OWNER: &str = "owner";

const MAX_PAIR_FAILS: u32 = 5;
const PAIR_LOCK_SECS: u64 = 5 * 60;

// ---- JWT claims ----

/// Device access token: `{ sub: device_id, acc: account_id, exp }` (PRD-06 §3).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccessClaims {
    pub sub: String, // device_id
    pub acc: String, // account_id
    pub exp: usize,
    #[serde(default)]
    pub typ: String,
}

/// Owner session token: `{ role: "owner", acc: account_id, exp }`. Minted by
/// login, spends only on pairing-code generation.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionClaims {
    pub acc: String,
    pub role: String,
    pub exp: usize,
    #[serde(default)]
    pub typ: String,
}

// ---- argon2 ----

/// argon2id hash of a secret (owner password, refresh secret, pairing code).
pub fn hash_secret(secret: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut ArgonRng);
    Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(ApiError::internal)
}

/// Constant-time-ish verify against a stored argon2 hash. Never panics; a
/// malformed stored hash simply fails to verify.
pub fn verify_secret(secret: &str, stored_hash: &str) -> bool {
    PasswordHash::new(stored_hash)
        .ok()
        .map(|parsed| {
            Argon2::default()
                .verify_password(secret.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

// ---- random material ----

/// 32 random bytes, hex-encoded — the device refresh secret (PRD-06 §3). Also
/// reused to seed a persisted JWT signing secret when none is configured.
pub fn generate_refresh_secret() -> String {
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    to_hex(&buf)
}

/// 8-char human-typable pairing code — Crockford-ish alphabet with the
/// ambiguous glyphs (0/O, 1/I) removed so it survives being read off a screen
/// and typed on a Steam Deck.
pub fn generate_pairing_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut buf = [0u8; 8];
    OsRng.fill_bytes(&mut buf);
    buf.iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ---- JWT mint / verify ----

/// Mint a device access JWT. `pub` so the pair/refresh handlers, the bootstrap,
/// and integration tests all mint through the same code path.
pub fn mint_access_token(secret: &[u8], account: Uuid, device: Uuid) -> Result<String, ApiError> {
    let exp = Utc::now().timestamp() as usize + ACCESS_TTL_SECS as usize;
    let claims = AccessClaims {
        sub: device.to_string(),
        acc: account.to_string(),
        exp,
        typ: TYP_ACCESS.to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(ApiError::internal)
}

/// Mint an owner session token (role=owner).
pub fn mint_owner_session(secret: &[u8], account: Uuid) -> Result<String, ApiError> {
    let exp = Utc::now().timestamp() as usize + SESSION_TTL_SECS as usize;
    let claims = SessionClaims {
        acc: account.to_string(),
        role: TYP_OWNER.to_string(),
        exp,
        typ: TYP_OWNER.to_string(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(ApiError::internal)
}

fn validation() -> Validation {
    let mut v = Validation::new(Algorithm::HS256);
    v.validate_exp = true;
    v.leeway = 5;
    v.set_required_spec_claims(&["exp"]);
    v
}

/// Verify a device access token and return its claims. Rejects expired / wrong
/// signature and, crucially, an owner session token presented as an access
/// token (the `typ` guard) — a session must never authorize a data call.
pub fn verify_access(secret: &[u8], token: &str) -> Result<AccessClaims, ApiError> {
    let data = decode::<AccessClaims>(token, &DecodingKey::from_secret(secret), &validation())
        .map_err(|_| ApiError::unauthorized("invalid or expired access token"))?;
    if data.claims.typ != TYP_ACCESS {
        return Err(ApiError::unauthorized("not an access token"));
    }
    Ok(data.claims)
}

/// Verify an owner session token. Rejects an access token presented as a session.
pub fn verify_owner_session(secret: &[u8], token: &str) -> Result<SessionClaims, ApiError> {
    let data = decode::<SessionClaims>(token, &DecodingKey::from_secret(secret), &validation())
        .map_err(|_| ApiError::unauthorized("invalid or expired session token"))?;
    if data.claims.typ != TYP_OWNER || data.claims.role != TYP_OWNER {
        return Err(ApiError::unauthorized("not an owner session"));
    }
    Ok(data.claims)
}

// ---- pairing rate-limit / lockout (PRD-06 §6) ----

/// In-process failed-attempt counter with lockout for device pairing.
///
/// ponytail: single-process, memory-resident (resets on restart) and account-
/// global rather than per-source-IP — right-sized for a single-owner home
/// server. Upgrade path: move the counter to the DB keyed by source IP + a
/// sliding window if the server is ever exposed to real internet traffic.
#[derive(Clone, Default)]
pub struct PairGuard {
    inner: Arc<Mutex<PairGuardInner>>,
}

#[derive(Default)]
struct PairGuardInner {
    fails: u32,
    locked_until: Option<Instant>,
}

impl PairGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reject with 429 while locked out.
    pub fn check(&self) -> Result<(), ApiError> {
        let g = self.inner.lock().unwrap();
        if let Some(until) = g.locked_until {
            if Instant::now() < until {
                return Err(ApiError::too_many(
                    "too many failed pairing attempts; try again later",
                ));
            }
        }
        Ok(())
    }

    pub fn record_fail(&self) {
        let mut g = self.inner.lock().unwrap();
        if let Some(until) = g.locked_until {
            if Instant::now() >= until {
                g.fails = 0;
                g.locked_until = None;
            }
        }
        g.fails += 1;
        if g.fails >= MAX_PAIR_FAILS {
            g.locked_until = Some(Instant::now() + Duration::from_secs(PAIR_LOCK_SECS));
        }
    }

    pub fn record_success(&self) {
        let mut g = self.inner.lock().unwrap();
        g.fails = 0;
        g.locked_until = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_roundtrips_and_rejects_session() {
        let secret = b"unit-test-secret";
        let acc = Uuid::now_v7();
        let dev = Uuid::now_v7();
        let tok = mint_access_token(secret, acc, dev).unwrap();
        let claims = verify_access(secret, &tok).unwrap();
        assert_eq!(claims.acc, acc.to_string());
        assert_eq!(claims.sub, dev.to_string());
        // A session token must not verify as an access token.
        let sess = mint_owner_session(secret, acc).unwrap();
        assert!(verify_access(secret, &sess).is_err());
        assert!(verify_owner_session(secret, &tok).is_err());
    }

    #[test]
    fn argon2_roundtrips() {
        let h = hash_secret("hunter2").unwrap();
        assert!(verify_secret("hunter2", &h));
        assert!(!verify_secret("wrong", &h));
        assert!(!verify_secret("hunter2", "not-a-hash"));
    }

    #[test]
    fn pair_guard_locks_out_after_threshold() {
        let g = PairGuard::new();
        assert!(g.check().is_ok());
        for _ in 0..MAX_PAIR_FAILS {
            g.record_fail();
        }
        assert!(g.check().is_err(), "should lock out after threshold");
        g.record_success();
        assert!(g.check().is_ok(), "success clears the lockout");
    }
}
