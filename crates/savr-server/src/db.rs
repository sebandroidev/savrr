//! Database access. Runtime `sqlx::query` (not the compile-time macros) so the
//! build needs no live database or offline metadata. The hot path is the
//! single-transaction CAS head-advance from PRD-04 §5 / PRD-05 §2.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use savr_core::protocol::HeadResponse;
use savr_core::{
    Blake3Hash, CreateVersion, Device, FileEntry, Os, Retention, SyncedConfig, Version, VersionKind,
};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::auth;
use crate::blobs::FsBlobStore;
use crate::error::ApiError;

/// Outcome of a version upload against the per-(account, game) head.
pub enum Advance {
    /// `parent == head`: head fast-forwarded to the new version.
    FastForward(Version),
    /// `parent != head`: new version stored as a divergent branch, head
    /// unchanged (PRD-03 §4). `head` is None only if the game had no head.
    Conflict {
        head: Option<Version>,
        incoming: Version,
    },
}

fn kind_to_str(k: VersionKind) -> &'static str {
    match k {
        VersionKind::Full => "full",
        VersionKind::Differential => "diff",
    }
}

fn parse_uuid(s: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(s).map_err(ApiError::internal)
}

fn parse_dt(s: &str) -> Result<DateTime<Utc>, ApiError> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(ApiError::internal)
}

fn row_to_version(r: &sqlx::sqlite::SqliteRow) -> Result<Version, ApiError> {
    let parent: Option<String> = r.get("parent");
    Ok(Version {
        id: parse_uuid(&r.get::<String, _>("id"))?,
        game_id: parse_uuid(&r.get::<String, _>("game_id"))?,
        device_id: parse_uuid(&r.get::<String, _>("device_id"))?,
        parent: parent.map(|s| parse_uuid(&s)).transpose()?,
        kind: match r.get::<String, _>("kind").as_str() {
            "diff" => VersionKind::Differential,
            _ => VersionKind::Full,
        },
        files: serde_json::from_str::<Vec<FileEntry>>(&r.get::<String, _>("files_json"))?,
        blob_hash: Blake3Hash::from_hex(&r.get::<String, _>("blob_hash"))
            .map_err(ApiError::internal)?,
        bytes: r.get::<i64, _>("bytes") as u64,
        seq: r.get::<i64, _>("seq") as u64,
        created_at: parse_dt(&r.get::<String, _>("created_at"))?,
    })
}

const VERSION_COLS: &str =
    "id, game_id, device_id, parent, kind, blob_hash, files_json, bytes, seq, created_at";

async fn load_version(pool: &SqlitePool, id: &str) -> Result<Version, ApiError> {
    let sql = format!("SELECT {VERSION_COLS} FROM versions WHERE id = ?");
    let row = sqlx::query(&sql).bind(id).fetch_one(pool).await?;
    row_to_version(&row)
}

/// Ensure a game exists for this account, returning its row as JSON.
pub async fn ensure_game(
    pool: &SqlitePool,
    account: Uuid,
    title: &str,
    steam_appid: Option<u32>,
) -> Result<Value, ApiError> {
    // Steam games carry a stable appid; dedup on it so a title that legitimately
    // varies across devices (Ludusavi manifest title vs .acf name, localization)
    // never mints a duplicate game that splits history. Custom games (no appid)
    // still dedup by title.
    // ponytail: the appid SELECT covers the steady state; the only gap is two
    // devices registering the same brand-new appid at the same instant. Add a
    // partial UNIQUE(account_id, steam_appid) index if that race ever bites.
    let existing = match steam_appid {
        Some(appid) => {
            sqlx::query(
                "SELECT id, title, steam_appid, head FROM games
             WHERE account_id = ? AND steam_appid = ?",
            )
            .bind(account.to_string())
            .bind(appid as i64)
            .fetch_optional(pool)
            .await?
        }
        None => None,
    };

    let row = match existing {
        Some(row) => row,
        None => {
            let id = Uuid::now_v7().to_string();
            sqlx::query(
                "INSERT INTO games (id, account_id, title, steam_appid) VALUES (?, ?, ?, ?)
                 ON CONFLICT(account_id, title) DO NOTHING",
            )
            .bind(&id)
            .bind(account.to_string())
            .bind(title)
            .bind(steam_appid.map(|a| a as i64))
            .execute(pool)
            .await?;

            sqlx::query(
                "SELECT id, title, steam_appid, head FROM games WHERE account_id = ? AND title = ?",
            )
            .bind(account.to_string())
            .bind(title)
            .fetch_one(pool)
            .await?
        }
    };
    Ok(json!({
        "id": row.get::<String, _>("id"),
        "title": row.get::<String, _>("title"),
        "steam_appid": row.get::<Option<i64>, _>("steam_appid"),
        "head": row.get::<Option<String>, _>("head"),
    }))
}

/// Head version id + seq for a game, or 404 if the game is unknown.
pub async fn get_head(
    pool: &SqlitePool,
    account: Uuid,
    game_id: Uuid,
) -> Result<HeadResponse, ApiError> {
    let row = sqlx::query(
        "SELECT g.head AS head, v.seq AS seq
         FROM games g LEFT JOIN versions v ON v.id = g.head
         WHERE g.id = ? AND g.account_id = ?",
    )
    .bind(game_id.to_string())
    .bind(account.to_string())
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("game"))?;
    let head = row
        .get::<Option<String>, _>("head")
        .map(|s| parse_uuid(&s))
        .transpose()?;
    let seq = row.get::<Option<i64>, _>("seq").map(|s| s as u64);
    Ok(HeadResponse { head, seq })
}

pub async fn list_versions(
    pool: &SqlitePool,
    account: Uuid,
    game_id: Uuid,
) -> Result<Vec<Version>, ApiError> {
    let sql = format!(
        "SELECT {VERSION_COLS} FROM versions
         WHERE game_id = ? AND account_id = ? ORDER BY seq DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(game_id.to_string())
        .bind(account.to_string())
        .fetch_all(pool)
        .await?;
    rows.iter().map(row_to_version).collect()
}

/// Insert a version and try to advance the head in one transaction. The
/// `head IS :expected_parent` update is a compare-and-swap: exactly one of two
/// racing devices fast-forwards; the other is stored as a branch (PRD-04 §5).
pub async fn create_version(
    pool: &SqlitePool,
    account: Uuid,
    game_id: Uuid,
    req: CreateVersion,
) -> Result<Advance, ApiError> {
    let mut tx = pool.begin().await?;

    let seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) + 1 FROM versions WHERE game_id = ?")
            .bind(game_id.to_string())
            .fetch_one(&mut *tx)
            .await?;

    let id = Uuid::now_v7().to_string();
    let parent = req.parent.map(|p| p.to_string());
    let created_at = Utc::now().to_rfc3339();
    let files_json = serde_json::to_string(&req.files)?;
    let blob_hex = req.blob_hash.to_hex();

    sqlx::query(
        "INSERT INTO versions
         (id, game_id, account_id, device_id, parent, kind, blob_hash, files_json, bytes, seq, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(game_id.to_string())
    .bind(account.to_string())
    .bind(req.device_id.to_string())
    .bind(&parent)
    .bind(kind_to_str(req.kind))
    .bind(&blob_hex)
    .bind(&files_json)
    .bind(req.bytes as i64)
    .bind(seq)
    .bind(&created_at)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE blobs SET refcount = refcount + 1 WHERE hash = ?")
        .bind(&blob_hex)
        .execute(&mut *tx)
        .await?;

    // Read the pre-existing head so we can report it on conflict.
    let prev_head: Option<String> =
        sqlx::query_scalar("SELECT head FROM games WHERE id = ? AND account_id = ?")
            .bind(game_id.to_string())
            .bind(account.to_string())
            .fetch_one(&mut *tx)
            .await?;

    // CAS: only advance if the head is still exactly what the client backed up
    // from. `IS` gives NULL-safe equality (first backup: parent NULL, head NULL).
    let advanced =
        sqlx::query("UPDATE games SET head = ? WHERE id = ? AND account_id = ? AND head IS ?")
            .bind(&id)
            .bind(game_id.to_string())
            .bind(account.to_string())
            .bind(&parent)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            == 1;

    tx.commit().await?;

    let incoming = load_version(pool, &id).await?;
    if advanced {
        Ok(Advance::FastForward(incoming))
    } else {
        let head = match prev_head {
            Some(h) => Some(load_version(pool, &h).await?),
            None => None,
        };
        Ok(Advance::Conflict { head, incoming })
    }
}

// ---- os mapping ----

fn os_str(os: Os) -> &'static str {
    match os {
        Os::Windows => "windows",
        Os::Linux => "linux",
        Os::Macos => "macos",
    }
}

fn os_from_str(s: &str) -> Os {
    Os::parse(s).unwrap_or(Os::Linux)
}

// ---- devices (PRD-06) ----

/// Insert a freshly paired device with its argon2-hashed refresh secret.
pub async fn create_device(
    pool: &SqlitePool,
    account: Uuid,
    device_id: Uuid,
    name: &str,
    os: Os,
    token_hash: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO devices (id, account_id, name, os, token_hash, last_seen, revoked)
         VALUES (?, ?, ?, ?, ?, ?, 0)",
    )
    .bind(device_id.to_string())
    .bind(account.to_string())
    .bind(name)
    .bind(os_str(os))
    .bind(token_hash)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// True iff the device exists on this account and is not revoked. Called by the
/// `Authed` extractor so revocation takes effect on the next request rather than
/// only when the short-lived access JWT expires.
pub async fn device_active(
    pool: &SqlitePool,
    account: Uuid,
    device: Uuid,
) -> Result<bool, ApiError> {
    let revoked: Option<i64> =
        sqlx::query_scalar("SELECT revoked FROM devices WHERE id = ? AND account_id = ?")
            .bind(device.to_string())
            .bind(account.to_string())
            .fetch_optional(pool)
            .await?;
    Ok(matches!(revoked, Some(0)))
}

/// (account, token_hash, revoked) for a device, for `POST /auth/refresh`.
pub async fn device_credentials(
    pool: &SqlitePool,
    device: Uuid,
) -> Result<Option<(Uuid, String, bool)>, ApiError> {
    let row = sqlx::query("SELECT account_id, token_hash, revoked FROM devices WHERE id = ?")
        .bind(device.to_string())
        .fetch_optional(pool)
        .await?;
    match row {
        Some(r) => Ok(Some((
            parse_uuid(&r.get::<String, _>("account_id"))?,
            r.get::<String, _>("token_hash"),
            r.get::<i64, _>("revoked") != 0,
        ))),
        None => Ok(None),
    }
}

pub async fn list_devices(pool: &SqlitePool, account: Uuid) -> Result<Vec<Device>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, name, os, last_seen FROM devices
         WHERE account_id = ? AND revoked = 0 ORDER BY name",
    )
    .bind(account.to_string())
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|r| {
            let last_seen = r
                .get::<Option<String>, _>("last_seen")
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());
            Ok(Device {
                id: parse_uuid(&r.get::<String, _>("id"))?,
                name: r.get::<String, _>("name"),
                os: os_from_str(&r.get::<String, _>("os")),
                last_seen,
            })
        })
        .collect()
}

/// Revoke a device (PRD-06 §3). Returns false if it does not belong to the account.
pub async fn revoke_device(
    pool: &SqlitePool,
    account: Uuid,
    device: Uuid,
) -> Result<bool, ApiError> {
    let n = sqlx::query("UPDATE devices SET revoked = 1 WHERE id = ? AND account_id = ?")
        .bind(device.to_string())
        .bind(account.to_string())
        .execute(pool)
        .await?
        .rows_affected();
    Ok(n == 1)
}

pub async fn touch_device(pool: &SqlitePool, account: Uuid, device: Uuid) -> Result<(), ApiError> {
    sqlx::query("UPDATE devices SET last_seen = ? WHERE id = ? AND account_id = ?")
        .bind(Utc::now().to_rfc3339())
        .bind(device.to_string())
        .bind(account.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

// ---- pairing codes (PRD-06 §2) ----

const MAX_ACTIVE_PAIRING_CODES: i64 = 5;

/// Mint a one-time pairing code: store its argon2 hash, return the plaintext
/// once. Generation is rate-limited by capping simultaneously-active codes.
pub async fn create_pairing_code(
    pool: &SqlitePool,
    account: Uuid,
) -> Result<(String, DateTime<Utc>), ApiError> {
    let now = Utc::now();
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pairing_codes WHERE account_id = ? AND used = 0 AND expires_at > ?",
    )
    .bind(account.to_string())
    .bind(now.to_rfc3339())
    .fetch_one(pool)
    .await?;
    if active >= MAX_ACTIVE_PAIRING_CODES {
        return Err(ApiError::too_many(
            "too many active pairing codes; wait for one to expire",
        ));
    }
    let code = auth::generate_pairing_code();
    let code_hash = auth::hash_secret(&code)?;
    let expires_at = now + chrono::Duration::seconds(auth::PAIRING_TTL_SECS);
    sqlx::query(
        "INSERT INTO pairing_codes (id, account_id, code_hash, expires_at, used, created_at)
         VALUES (?, ?, ?, ?, 0, ?)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(account.to_string())
    .bind(&code_hash)
    .bind(expires_at.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;
    Ok((code, expires_at))
}

/// Validate and burn a pairing code. Codes are stored hashed, so we scan the
/// (small, single-owner) set of active codes and argon2-verify each; on a match
/// we single-use-burn it with a CAS and return its account. Returns None for no
/// match (caller records a failed attempt for lockout).
pub async fn redeem_pairing_code(pool: &SqlitePool, code: &str) -> Result<Option<Uuid>, ApiError> {
    let now = Utc::now().to_rfc3339();
    let rows = sqlx::query(
        "SELECT id, account_id, code_hash FROM pairing_codes WHERE used = 0 AND expires_at > ?",
    )
    .bind(&now)
    .fetch_all(pool)
    .await?;
    for r in rows {
        let hash: String = r.get("code_hash");
        if auth::verify_secret(code, &hash) {
            let id: String = r.get("id");
            let burned = sqlx::query("UPDATE pairing_codes SET used = 1 WHERE id = ? AND used = 0")
                .bind(&id)
                .execute(pool)
                .await?
                .rows_affected()
                == 1;
            if burned {
                return Ok(Some(parse_uuid(&r.get::<String, _>("account_id"))?));
            }
        }
    }
    Ok(None)
}

// ---- synced config (PRD-04 /config) ----

/// Fetch the account config, seeding a default row on first access. The `tag`
/// is always taken from the stored column (authoritative for concurrency).
pub async fn get_or_seed_config(
    pool: &SqlitePool,
    account: Uuid,
) -> Result<SyncedConfig, ApiError> {
    if let Some(cfg) = read_config(pool, account).await? {
        return Ok(cfg);
    }
    seed_default_config(pool, account).await
}

async fn read_config(pool: &SqlitePool, account: Uuid) -> Result<Option<SyncedConfig>, ApiError> {
    let row = sqlx::query("SELECT tag, data_json FROM config WHERE account_id = ?")
        .bind(account.to_string())
        .fetch_optional(pool)
        .await?;
    match row {
        Some(r) => {
            let mut cfg: SyncedConfig = serde_json::from_str(&r.get::<String, _>("data_json"))?;
            cfg.tag = r.get::<String, _>("tag");
            Ok(Some(cfg))
        }
        None => Ok(None),
    }
}

/// Seed the default config for an account (idempotent). Called at owner
/// bootstrap and lazily on first `/config` access.
pub async fn seed_default_config(
    pool: &SqlitePool,
    account: Uuid,
) -> Result<SyncedConfig, ApiError> {
    let tag = Uuid::now_v7().to_string();
    let cfg = SyncedConfig {
        tag: tag.clone(),
        ..Default::default()
    };
    let data = serde_json::to_string(&cfg)?;
    sqlx::query("INSERT OR IGNORE INTO config (account_id, tag, data_json) VALUES (?, ?, ?)")
        .bind(account.to_string())
        .bind(&tag)
        .bind(&data)
        .execute(pool)
        .await?;
    // Re-read: a concurrent seed may have won, and we want its tag.
    read_config(pool, account)
        .await?
        .ok_or_else(|| ApiError::internal("config vanished after seed"))
}

/// Optimistic-concurrency update (PRD-04 §2): reject with 409 unless the
/// client's `tag` matches the stored tag, then assign a fresh tag and persist.
pub async fn put_config(
    pool: &SqlitePool,
    account: Uuid,
    incoming: SyncedConfig,
) -> Result<SyncedConfig, ApiError> {
    let stored_tag =
        match sqlx::query_scalar::<_, String>("SELECT tag FROM config WHERE account_id = ?")
            .bind(account.to_string())
            .fetch_optional(pool)
            .await?
        {
            Some(t) => t,
            None => seed_default_config(pool, account).await?.tag,
        };

    if incoming.tag != stored_tag {
        return Err(ApiError::config_conflict(&stored_tag));
    }

    let new_tag = Uuid::now_v7().to_string();
    let mut cfg = incoming;
    cfg.tag = new_tag.clone();
    let data = serde_json::to_string(&cfg)?;

    // Tag-guarded UPDATE = CAS: a racing writer that already rotated the tag
    // makes this affect 0 rows, so we surface the conflict instead of clobbering.
    let n =
        sqlx::query("UPDATE config SET tag = ?, data_json = ? WHERE account_id = ? AND tag = ?")
            .bind(&new_tag)
            .bind(&data)
            .bind(account.to_string())
            .bind(&stored_tag)
            .execute(pool)
            .await?
            .rows_affected();
    if n != 1 {
        let cur: Option<String> = sqlx::query_scalar("SELECT tag FROM config WHERE account_id = ?")
            .bind(account.to_string())
            .fetch_optional(pool)
            .await?;
        return Err(ApiError::config_conflict(&cur.unwrap_or_default()));
    }
    Ok(cfg)
}

// ---- conflict resolution (PRD-03 §4) ----

/// Set `winner` as the new head (CAS-safe against a concurrent move) and record
/// the resolution. The previous head becomes the loser — kept as a branch (its
/// version row is never deleted here); `keep_both` records a sibling-folder
/// redirect note for restore. Returns the new head + winner seq.
pub async fn resolve_conflict(
    pool: &SqlitePool,
    account: Uuid,
    game_id: Uuid,
    winner: Uuid,
    keep_both: bool,
) -> Result<(HeadResponse, u64), ApiError> {
    let mut tx = pool.begin().await?;

    let cur_head: Option<String> =
        sqlx::query_scalar("SELECT head FROM games WHERE id = ? AND account_id = ?")
            .bind(game_id.to_string())
            .bind(account.to_string())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| ApiError::not_found("game"))?;

    let winner_seq: i64 = sqlx::query_scalar(
        "SELECT seq FROM versions WHERE id = ? AND game_id = ? AND account_id = ?",
    )
    .bind(winner.to_string())
    .bind(game_id.to_string())
    .bind(account.to_string())
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::not_found("winner version"))?;

    let advanced =
        sqlx::query("UPDATE games SET head = ? WHERE id = ? AND account_id = ? AND head IS ?")
            .bind(winner.to_string())
            .bind(game_id.to_string())
            .bind(account.to_string())
            .bind(&cur_head)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            == 1;
    if !advanced {
        tx.rollback().await?;
        return Err(ApiError::conflict(
            json!(cur_head),
            json!(winner.to_string()),
        ));
    }

    // Record the loser (previous head) unless the winner already was the head.
    if let Some(loser) = cur_head.as_ref() {
        if loser != &winner.to_string() {
            let redirect = if keep_both {
                Some(format!("restore-conflict-{}", &loser[..8.min(loser.len())]))
            } else {
                None
            };
            sqlx::query(
                "INSERT INTO resolved_conflicts
                 (id, account_id, game_id, winner, loser, keep_both, redirect, resolved_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(account.to_string())
            .bind(game_id.to_string())
            .bind(winner.to_string())
            .bind(loser)
            .bind(keep_both as i64)
            .bind(&redirect)
            .bind(Utc::now().to_rfc3339())
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;
    Ok((
        HeadResponse {
            head: Some(winner),
            seq: Some(winner_seq as u64),
        },
        winner_seq as u64,
    ))
}

// ---- blob access gate + WS replay ----

/// True iff this account has a version referencing `blob_hash` — the blob-access
/// gate for `GET /blobs/{hash}` (PRD-06 §6), blocking cross-account hash guessing.
pub async fn account_references_blob(
    pool: &SqlitePool,
    account: Uuid,
    blob_hash: &str,
) -> Result<bool, ApiError> {
    let hit: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM versions WHERE account_id = ? AND blob_hash = ? LIMIT 1")
            .bind(account.to_string())
            .bind(blob_hash)
            .fetch_optional(pool)
            .await?;
    Ok(hit.is_some())
}

/// Versions with `seq` greater than the device's per-game `last_seq` — the
/// offline catch-up replay for `Hello` (PRD-03 §5). Games absent from the map
/// use a threshold of 0 (replay all).
pub async fn versions_since(
    pool: &SqlitePool,
    account: Uuid,
    last_seq: &HashMap<Uuid, u64>,
) -> Result<Vec<(Uuid, Uuid, u64)>, ApiError> {
    let rows = sqlx::query(
        "SELECT game_id, id, seq FROM versions WHERE account_id = ? ORDER BY game_id, seq",
    )
    .bind(account.to_string())
    .fetch_all(pool)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        let game = parse_uuid(&r.get::<String, _>("game_id"))?;
        let version = parse_uuid(&r.get::<String, _>("id"))?;
        let seq = r.get::<i64, _>("seq") as u64;
        if seq > last_seq.get(&game).copied().unwrap_or(0) {
            out.push((game, version, seq));
        }
    }
    Ok(out)
}

// ---- retention GC (PRD-03 §7) ----

#[derive(Debug, Default, Clone, Copy)]
pub struct GcStats {
    pub versions_pruned: u64,
    pub blobs_deleted: u64,
}

/// Retention GC across every account+game (PRD-03 §7): keep the newest `full`
/// full versions plus `diff_per_full` newest diffs per kept full; prune the
/// rest, deref their blobs, and delete blobs whose refcount hits zero. The
/// current head and every *unresolved* conflict tip are always protected.
///
/// Safe to prune middle/old diffs within a kept full because a differential is
/// taken against the last *full*, not the previous diff (PRD-03 §2) — so diffs
/// in a group are mutually independent for restore.
pub async fn run_gc(pool: &SqlitePool, blobs: &FsBlobStore) -> Result<GcStats, ApiError> {
    let mut stats = GcStats::default();
    let accounts: Vec<String> = sqlx::query_scalar("SELECT id FROM accounts")
        .fetch_all(pool)
        .await?;
    for acc_s in accounts {
        let account = parse_uuid(&acc_s)?;
        let retention = config_retention(pool, account).await?;
        let games: Vec<String> = sqlx::query_scalar("SELECT id FROM games WHERE account_id = ?")
            .bind(account.to_string())
            .fetch_all(pool)
            .await?;
        for game_s in games {
            let head: Option<String> =
                sqlx::query_scalar("SELECT head FROM games WHERE id = ? AND account_id = ?")
                    .bind(&game_s)
                    .bind(account.to_string())
                    .fetch_optional(pool)
                    .await?
                    .flatten();
            prune_game(
                pool,
                blobs,
                account,
                &game_s,
                head.as_deref(),
                retention,
                &mut stats,
            )
            .await?;
        }
    }
    Ok(stats)
}

async fn config_retention(pool: &SqlitePool, account: Uuid) -> Result<Retention, ApiError> {
    Ok(read_config(pool, account)
        .await?
        .map(|c| c.retention)
        .unwrap_or_default())
}

struct GcVersion {
    id: String,
    kind: String,
    blob_hash: String,
}

#[allow(clippy::too_many_arguments)]
async fn prune_game(
    pool: &SqlitePool,
    blobs: &FsBlobStore,
    account: Uuid,
    game_s: &str,
    head: Option<&str>,
    retention: Retention,
    stats: &mut GcStats,
) -> Result<(), ApiError> {
    let rows = sqlx::query(
        "SELECT id, kind, blob_hash FROM versions
         WHERE game_id = ? AND account_id = ? ORDER BY seq ASC",
    )
    .bind(game_s)
    .bind(account.to_string())
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let vers: Vec<GcVersion> = rows
        .iter()
        .map(|r| GcVersion {
            id: r.get::<String, _>("id"),
            kind: r.get::<String, _>("kind"),
            blob_hash: r.get::<String, _>("blob_hash"),
        })
        .collect();

    // Nearest preceding full for each version (index into `vers`); a full owns
    // itself. A leading diff with no preceding full has `None` and is treated as
    // its own anchor.
    let mut owning: Vec<Option<usize>> = vec![None; vers.len()];
    let mut last_full: Option<usize> = None;
    for (i, v) in vers.iter().enumerate() {
        if v.kind == "full" {
            last_full = Some(i);
            owning[i] = Some(i);
        } else {
            owning[i] = last_full;
        }
    }

    // Protected set: head + every unresolved conflict tip (a tip = not any
    // version's parent; unresolved = not recorded as a resolved loser).
    let referenced: HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT parent FROM versions
         WHERE game_id = ? AND account_id = ? AND parent IS NOT NULL",
    )
    .bind(game_s)
    .bind(account.to_string())
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    let resolved_losers: HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT loser FROM resolved_conflicts WHERE game_id = ? AND account_id = ?",
    )
    .bind(game_s)
    .bind(account.to_string())
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    let mut keep = vec![false; vers.len()];
    for (i, v) in vers.iter().enumerate() {
        let is_head = head == Some(v.id.as_str());
        let is_tip = !referenced.contains(&v.id);
        if is_head || (is_tip && !resolved_losers.contains(&v.id)) {
            keep[i] = true;
            if let Some(f) = owning[i] {
                keep[f] = true; // keep the owning full so the tip stays restorable
            }
        }
    }

    // Keep the newest N fulls (by seq; `vers` is ascending so newest = last).
    let full_idxs: Vec<usize> = (0..vers.len())
        .filter(|&i| vers[i].kind == "full")
        .collect();
    let keep_from = full_idxs.len().saturating_sub(retention.full as usize);
    let kept_fulls: HashSet<usize> = full_idxs[keep_from..].iter().copied().collect();
    for &f in &kept_fulls {
        keep[f] = true;
    }

    // Within each kept full, keep the newest M diffs.
    let mut diffs_by_full: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, v) in vers.iter().enumerate() {
        if v.kind != "full" {
            if let Some(f) = owning[i] {
                diffs_by_full.entry(f).or_default().push(i);
            }
        }
    }
    for (f, group) in &diffs_by_full {
        if kept_fulls.contains(f) {
            let start = group.len().saturating_sub(retention.diff_per_full as usize);
            for &i in &group[start..] {
                keep[i] = true;
            }
        }
    }

    for (i, v) in vers.iter().enumerate() {
        if !keep[i] {
            delete_version_and_deref(pool, blobs, &v.id, &v.blob_hash, stats).await?;
        }
    }
    Ok(())
}

/// Delete one version row, decrement its blob's refcount, and if that reaches
/// zero delete the blob row + object. The blobs table refcount (not the prune
/// set) is the source of truth for shared blobs, so dedup stays correct.
async fn delete_version_and_deref(
    pool: &SqlitePool,
    blobs: &FsBlobStore,
    version_id: &str,
    blob_hash: &str,
    stats: &mut GcStats,
) -> Result<(), ApiError> {
    let mut tx = pool.begin().await?;
    // Null any surviving `parent` pointer into this row before deleting it: the
    // self-FK is enforced, and a differential restores against the last *full*
    // (PRD-03 §2), so the lineage link is safe to drop once its target is pruned.
    sqlx::query("UPDATE versions SET parent = NULL WHERE parent = ?")
        .bind(version_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM versions WHERE id = ?")
        .bind(version_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE blobs SET refcount = refcount - 1 WHERE hash = ?")
        .bind(blob_hash)
        .execute(&mut *tx)
        .await?;
    let refcount: Option<i64> = sqlx::query_scalar("SELECT refcount FROM blobs WHERE hash = ?")
        .bind(blob_hash)
        .fetch_optional(&mut *tx)
        .await?;
    let delete_blob = matches!(refcount, Some(r) if r <= 0);
    if delete_blob {
        sqlx::query("DELETE FROM blobs WHERE hash = ?")
            .bind(blob_hash)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    stats.versions_pruned += 1;
    if delete_blob {
        blobs.delete(blob_hash).await.map_err(ApiError::internal)?;
        stats.blobs_deleted += 1;
    }
    Ok(())
}
