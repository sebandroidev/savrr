//! `savr-server` — Axum service: versioned save history, content-addressed blob
//! store, CAS head-advance (PRD-04). Exposed as a lib so integration tests can
//! drive the router in-process.

use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

pub mod api;
pub mod auth;
pub mod blobs;
pub mod db;
pub mod error;
pub mod hub;
pub mod ws;

pub use api::{build_app, AppState};
pub use blobs::FsBlobStore;
pub use hub::Hub;

/// Open (and create if missing) the SQLite pool.
pub async fn connect(db_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::from_str(db_url)?.create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await
}

/// Run checked-in migrations (forward-only, idempotent — PRD-07 §5).
pub async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}
