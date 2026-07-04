use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use savr_server::{auth, build_app, connect, db, migrate, AppState, FsBlobStore};
use uuid::Uuid;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// HS256 signing secret (PRD-06 §3). Prefer `SAVR_JWT_SECRET`; otherwise reuse a
/// generated-and-persisted secret so tokens survive restarts.
fn load_or_create_jwt_secret() -> Vec<u8> {
    if let Ok(s) = std::env::var("SAVR_JWT_SECRET") {
        if !s.is_empty() {
            return s.into_bytes();
        }
    }
    let path = env("SAVR_JWT_SECRET_FILE", "data/jwt.secret");
    if let Ok(bytes) = std::fs::read(&path) {
        if !bytes.is_empty() {
            return bytes;
        }
    }
    let secret = auth::generate_refresh_secret(); // 64 hex chars of CSPRNG entropy
    if let Some(parent) = Path::new(&path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if std::fs::write(&path, secret.as_bytes()).is_err() {
        tracing::warn!("could not persist JWT secret to {path}; tokens will not survive restart");
    }
    secret.into_bytes()
}

/// Single-owner bootstrap (PRD-06 §1–2): seed exactly one account on first boot,
/// with `owner_hash = argon2(SAVR_OWNER_PASSWORD)`, and a default synced config.
/// If the password env is unset, generate one and log it once so the operator
/// can set it deliberately later.
async fn ensure_owner_account(pool: &sqlx::SqlitePool) -> Result<Uuid, Box<dyn std::error::Error>> {
    if let Some(id) = sqlx::query_scalar::<_, String>("SELECT id FROM accounts LIMIT 1")
        .fetch_optional(pool)
        .await?
    {
        return Ok(Uuid::parse_str(&id)?);
    }

    let password = match std::env::var("SAVR_OWNER_PASSWORD") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            let generated = auth::generate_refresh_secret();
            tracing::warn!(
                "SAVR_OWNER_PASSWORD unset — generated a temporary owner password: {generated} \
                 (set SAVR_OWNER_PASSWORD and restart to choose your own)"
            );
            generated
        }
    };
    let owner_hash = auth::hash_secret(&password).map_err(|e| e.message)?;

    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, owner_hash, created_at) VALUES (?, ?, ?)")
        .bind(id.to_string())
        .bind(&owner_hash)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
    db::seed_default_config(pool, id)
        .await
        .map_err(|e| e.message)?;
    tracing::info!("seeded owner account {id} (PRD-06 bootstrap)");
    Ok(id)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let db_url = env("SAVR_DB_URL", "sqlite://data/savr.db");
    let blob_path = env("SAVR_BLOB_PATH", "data/blobs");
    let bind = env("SAVR_BIND", "0.0.0.0:8080");
    let gc_interval: u64 = env("SAVR_GC_INTERVAL_SECS", "3600").parse().unwrap_or(3600);

    // Local file backends need their dirs to exist (create_if_missing only
    // makes the db file, not its parent).
    if !db_url.contains(":memory:") {
        std::fs::create_dir_all("data").ok();
    }
    std::fs::create_dir_all(&blob_path).ok();

    let pool = connect(&db_url).await?;
    migrate(&pool).await?;
    ensure_owner_account(&pool).await?;

    let jwt_secret = load_or_create_jwt_secret();
    let state = AppState::new(pool, FsBlobStore::new(&blob_path), jwt_secret);

    // Retention GC on a schedule (PRD-03 §7).
    let gc_state = state.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(gc_interval));
        ticker.tick().await; // skip the immediate first tick
        loop {
            ticker.tick().await;
            match db::run_gc(&gc_state.pool, &gc_state.blobs).await {
                Ok(stats) if stats.versions_pruned > 0 => {
                    tracing::info!(
                        "retention gc pruned {} versions, {} blobs",
                        stats.versions_pruned,
                        stats.blobs_deleted
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("retention gc failed: {}", e.message),
            }
        }
    });

    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("savr-server listening on {bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
