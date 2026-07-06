//! Local daemon state (PRD-05 §3): its own SQLite DB, separate from the
//! server's. Holds the last snapshot per game (for diffing), the exe→game
//! index, the offline upload outbox, registered roots, and a small key/value
//! `meta` table for the account id, synced config, and last-backup timestamp.
//!
//! Uses runtime `sqlx::query` (not the compile-time macros) so the build needs
//! no live database or offline metadata — same choice the server made.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use savr_core::ipc::{Root, RootKind};
use savr_core::{FileEntry, GameId, SyncedConfig, VersionId};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS local_snapshots (
    game_id    TEXT PRIMARY KEY,
    files_json TEXT NOT NULL,
    registry   BLOB,
    taken_at   TEXT NOT NULL,
    local_head TEXT
);
CREATE TABLE IF NOT EXISTS exe_index (
    exe_key    TEXT PRIMARY KEY,
    game_id    TEXT NOT NULL,
    confidence INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS outbox (
    version_id TEXT PRIMARY KEY,
    payload    BLOB NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    next_retry TEXT
);
CREATE TABLE IF NOT EXISTS roots (
    id   TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    path TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS play_stats (
    game_id           TEXT PRIMARY KEY,
    session_start     TEXT,
    last_played       TEXT,
    last_session_secs INTEGER,
    total_secs        INTEGER NOT NULL DEFAULT 0
);
"#;

/// Last snapshot stored for a game, plus the version this device last synced.
#[derive(Debug, Clone)]
pub struct StoredSnapshot {
    pub files: Vec<FileEntry>,
    pub taken_at: DateTime<Utc>,
    pub local_head: Option<VersionId>,
}

/// Per-game play tracking, derived from the daemon's `GameStarted`/`GameStopped`
/// detection events. Exists to prove detection is firing even for games Savr
/// can't back up yet (no known save location).
#[derive(Debug, Clone, Default)]
pub struct PlayStat {
    pub last_played: Option<DateTime<Utc>>,
    pub last_session_secs: Option<i64>,
    pub total_secs: i64,
}

/// A queued upload awaiting a reachable server (PRD-03 §8).
#[derive(Debug, Clone)]
pub struct OutboxItem {
    pub version_id: VersionId,
    pub payload: Vec<u8>,
    pub attempts: i64,
}

#[derive(Clone)]
pub struct LocalState {
    pool: SqlitePool,
}

impl LocalState {
    /// Open (creating if missing) the local state DB at `path`, applying the
    /// idempotent schema on boot.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        let state = Self { pool };
        state.init().await?;
        Ok(state)
    }

    /// In-memory DB for tests (single connection so the shared cache persists).
    pub async fn open_memory() -> anyhow::Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        let state = Self { pool };
        state.init().await?;
        Ok(state)
    }

    async fn init(&self) -> anyhow::Result<()> {
        // `execute_many` is gone in sqlx 0.8; run statements one by one.
        for stmt in SCHEMA.split(';') {
            let stmt = stmt.trim();
            if !stmt.is_empty() {
                sqlx::query(stmt).execute(&self.pool).await?;
            }
        }
        Ok(())
    }

    // ---- snapshots ----

    pub async fn get_snapshot(&self, game_id: GameId) -> anyhow::Result<Option<StoredSnapshot>> {
        let row = sqlx::query(
            "SELECT files_json, taken_at, local_head FROM local_snapshots WHERE game_id = ?",
        )
        .bind(game_id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let files: Vec<FileEntry> = serde_json::from_str(&row.get::<String, _>("files_json"))?;
        let taken_at =
            DateTime::parse_from_rfc3339(&row.get::<String, _>("taken_at"))?.with_timezone(&Utc);
        let local_head = row
            .get::<Option<String>, _>("local_head")
            .map(|s| Uuid::parse_str(&s))
            .transpose()?;
        Ok(Some(StoredSnapshot {
            files,
            taken_at,
            local_head,
        }))
    }

    /// Upsert the last snapshot + local head for a game.
    pub async fn put_snapshot(
        &self,
        game_id: GameId,
        files: &[FileEntry],
        taken_at: DateTime<Utc>,
        local_head: Option<VersionId>,
    ) -> anyhow::Result<()> {
        let files_json = serde_json::to_string(files)?;
        sqlx::query(
            "INSERT INTO local_snapshots (game_id, files_json, taken_at, local_head)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(game_id) DO UPDATE SET
                 files_json = excluded.files_json,
                 taken_at   = excluded.taken_at,
                 local_head = excluded.local_head",
        )
        .bind(game_id.to_string())
        .bind(files_json)
        .bind(taken_at.to_rfc3339())
        .bind(local_head.map(|v| v.to_string()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Advance just the local head after a version is accepted server-side.
    pub async fn set_local_head(&self, game_id: GameId, head: VersionId) -> anyhow::Result<()> {
        sqlx::query("UPDATE local_snapshots SET local_head = ? WHERE game_id = ?")
            .bind(head.to_string())
            .bind(game_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- exe index ----

    pub async fn load_exe_rows(&self) -> anyhow::Result<Vec<(String, GameId, i64)>> {
        let rows = sqlx::query("SELECT exe_key, game_id, confidence FROM exe_index")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let game_id = Uuid::parse_str(&r.get::<String, _>("game_id"))?;
            out.push((
                r.get::<String, _>("exe_key"),
                game_id,
                r.get::<i64, _>("confidence"),
            ));
        }
        Ok(out)
    }

    /// Replace the whole exe index atomically (rebuilt on root/game changes).
    pub async fn replace_exe_index(&self, rows: &[(String, GameId, i64)]) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM exe_index")
            .execute(&mut *tx)
            .await?;
        for (key, game_id, conf) in rows {
            sqlx::query(
                "INSERT OR REPLACE INTO exe_index (exe_key, game_id, confidence) VALUES (?, ?, ?)",
            )
            .bind(key)
            .bind(game_id.to_string())
            .bind(conf)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ---- outbox ----

    pub async fn enqueue_outbox(
        &self,
        version_id: VersionId,
        payload: &[u8],
        next_retry: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO outbox (version_id, payload, attempts, next_retry)
             VALUES (?, ?, COALESCE((SELECT attempts FROM outbox WHERE version_id = ?), 0), ?)",
        )
        .bind(version_id.to_string())
        .bind(payload)
        .bind(version_id.to_string())
        .bind(next_retry.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Items whose `next_retry` is due (or null), oldest first.
    pub async fn due_outbox(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<OutboxItem>> {
        let rows = sqlx::query(
            "SELECT version_id, payload, attempts FROM outbox
             WHERE next_retry IS NULL OR next_retry <= ?
             ORDER BY next_retry ASC",
        )
        .bind(now.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(OutboxItem {
                version_id: Uuid::parse_str(&r.get::<String, _>("version_id"))?,
                payload: r.get::<Vec<u8>, _>("payload"),
                attempts: r.get::<i64, _>("attempts"),
            });
        }
        Ok(out)
    }

    pub async fn bump_outbox_attempt(
        &self,
        version_id: VersionId,
        next_retry: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE outbox SET attempts = attempts + 1, next_retry = ? WHERE version_id = ?",
        )
        .bind(next_retry.to_rfc3339())
        .bind(version_id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_outbox(&self, version_id: VersionId) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM outbox WHERE version_id = ?")
            .bind(version_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn outbox_count(&self) -> anyhow::Result<u32> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM outbox")
            .fetch_one(&self.pool)
            .await?;
        Ok(n as u32)
    }

    // ---- roots ----

    pub async fn add_root(&self, kind: RootKind, path: &str) -> anyhow::Result<Root> {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO roots (id, kind, path) VALUES (?, ?, ?)")
            .bind(id.to_string())
            .bind(root_kind_str(kind))
            .bind(path)
            .execute(&self.pool)
            .await?;
        Ok(Root {
            id,
            kind,
            path: path.to_string(),
        })
    }

    pub async fn remove_root(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM roots WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_roots(&self) -> anyhow::Result<Vec<Root>> {
        let rows = sqlx::query("SELECT id, kind, path FROM roots ORDER BY path")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(Root {
                id: Uuid::parse_str(&r.get::<String, _>("id"))?,
                kind: root_kind_from_str(&r.get::<String, _>("kind")),
                path: r.get::<String, _>("path"),
            });
        }
        Ok(out)
    }

    // ---- meta ----

    pub async fn get_meta(&self, key: &str) -> anyhow::Result<Option<String>> {
        let v: Option<String> = sqlx::query_scalar("SELECT value FROM meta WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(v)
    }

    pub async fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO meta (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- play stats ----

    /// Open a play session: stamp `last_played` and remember the start so the
    /// matching stop can measure duration. Upsert so a game seen for the first
    /// time gets a row.
    pub async fn play_start(&self, game_id: GameId, at: DateTime<Utc>) -> anyhow::Result<()> {
        let at = at.to_rfc3339();
        sqlx::query(
            "INSERT INTO play_stats (game_id, session_start, last_played, total_secs)
             VALUES (?, ?, ?, 0)
             ON CONFLICT(game_id) DO UPDATE SET session_start = excluded.session_start,
                                                last_played   = excluded.last_played",
        )
        .bind(game_id.to_string())
        .bind(&at)
        .bind(&at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Close a play session: add its duration to the total and record it as the
    /// last session. If no session was open (the daemon restarted mid-play) we
    /// can't measure duration, so just refresh `last_played`.
    pub async fn play_stop(&self, game_id: GameId, at: DateTime<Utc>) -> anyhow::Result<()> {
        let row = sqlx::query("SELECT session_start FROM play_stats WHERE game_id = ?")
            .bind(game_id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        let start = row.and_then(|r| r.get::<Option<String>, _>("session_start"));
        let ended = at.to_rfc3339();
        match start.and_then(|s| DateTime::parse_from_rfc3339(&s).ok()) {
            Some(started) => {
                // Clamp: a backwards clock must never subtract from the total.
                let secs = (at - started.with_timezone(&Utc)).num_seconds().max(0);
                sqlx::query(
                    "UPDATE play_stats
                        SET session_start = NULL, last_played = ?,
                            last_session_secs = ?, total_secs = total_secs + ?
                      WHERE game_id = ?",
                )
                .bind(&ended)
                .bind(secs)
                .bind(secs)
                .bind(game_id.to_string())
                .execute(&self.pool)
                .await?;
            }
            None => {
                sqlx::query(
                    "INSERT INTO play_stats (game_id, last_played, total_secs) VALUES (?, ?, 0)
                     ON CONFLICT(game_id) DO UPDATE SET last_played = excluded.last_played",
                )
                .bind(game_id.to_string())
                .bind(&ended)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }

    /// Clear any session left open by a daemon that died mid-play. The real stop
    /// time went unobserved (the daemon wasn't running to see it), so no duration
    /// can be credited — just drop the marker so a stale `session_start` doesn't
    /// pin `last_played` to the start time until the game's next launch.
    pub async fn close_orphaned_sessions(&self) -> anyhow::Result<()> {
        sqlx::query("UPDATE play_stats SET session_start = NULL WHERE session_start IS NOT NULL")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// All play stats, keyed by game, for overlaying onto the games list.
    pub async fn play_stats(&self) -> anyhow::Result<HashMap<GameId, PlayStat>> {
        let rows =
            sqlx::query("SELECT game_id, last_played, last_session_secs, total_secs FROM play_stats")
                .fetch_all(&self.pool)
                .await?;
        let mut out = HashMap::new();
        for row in rows {
            let Ok(id) = Uuid::parse_str(&row.get::<String, _>("game_id")) else {
                continue;
            };
            let last_played = row
                .get::<Option<String>, _>("last_played")
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|d| d.with_timezone(&Utc));
            out.insert(
                id,
                PlayStat {
                    last_played,
                    last_session_secs: row.get::<Option<i64>, _>("last_session_secs"),
                    total_secs: row.get::<i64, _>("total_secs"),
                },
            );
        }
        Ok(out)
    }

    /// The synced account config, if the daemon has fetched/stored one.
    pub async fn synced_config(&self) -> anyhow::Result<Option<SyncedConfig>> {
        match self.get_meta("synced_config").await? {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    pub async fn set_synced_config(&self, cfg: &SyncedConfig) -> anyhow::Result<()> {
        self.set_meta("synced_config", &serde_json::to_string(cfg)?)
            .await
    }

    pub async fn last_backup_at(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        match self.get_meta("last_backup_at").await? {
            Some(s) => Ok(Some(DateTime::parse_from_rfc3339(&s)?.with_timezone(&Utc))),
            None => Ok(None),
        }
    }

    pub async fn set_last_backup_at(&self, at: DateTime<Utc>) -> anyhow::Result<()> {
        self.set_meta("last_backup_at", &at.to_rfc3339()).await
    }
}

fn root_kind_str(kind: RootKind) -> &'static str {
    match kind {
        RootKind::Steam => "steam",
        RootKind::Drive => "drive",
        RootKind::Emulator => "emulator",
        RootKind::Launcher => "launcher",
    }
}

fn root_kind_from_str(s: &str) -> RootKind {
    match s {
        "drive" => RootKind::Drive,
        "emulator" => RootKind::Emulator,
        "launcher" => RootKind::Launcher,
        _ => RootKind::Steam,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savr_core::hash::Blake3Hash;

    #[tokio::test]
    async fn roots_and_meta_roundtrip() {
        let state = LocalState::open_memory().await.unwrap();
        assert!(state.list_roots().await.unwrap().is_empty());

        let root = state
            .add_root(RootKind::Steam, "/home/me/.steam")
            .await
            .unwrap();
        let roots = state.list_roots().await.unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].kind, RootKind::Steam);

        state.remove_root(root.id).await.unwrap();
        assert!(state.list_roots().await.unwrap().is_empty());

        state.set_meta("k", "v").await.unwrap();
        assert_eq!(state.get_meta("k").await.unwrap().as_deref(), Some("v"));
    }

    #[tokio::test]
    async fn snapshot_and_outbox_roundtrip() {
        let state = LocalState::open_memory().await.unwrap();
        let game = Uuid::now_v7();
        assert!(state.get_snapshot(game).await.unwrap().is_none());

        let files = vec![FileEntry {
            rel_path: "a.sav".into(),
            size: 3,
            mtime: 0,
            hash: Blake3Hash::of(b"abc"),
        }];
        let head = Uuid::now_v7();
        state
            .put_snapshot(game, &files, Utc::now(), Some(head))
            .await
            .unwrap();
        let stored = state.get_snapshot(game).await.unwrap().unwrap();
        assert_eq!(stored.files, files);
        assert_eq!(stored.local_head, Some(head));

        let vid = Uuid::now_v7();
        state
            .enqueue_outbox(vid, b"payload", Utc::now())
            .await
            .unwrap();
        assert_eq!(state.outbox_count().await.unwrap(), 1);
        let due = state.due_outbox(Utc::now()).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].payload, b"payload");
        state.remove_outbox(vid).await.unwrap();
        assert_eq!(state.outbox_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn play_sessions_accumulate() {
        use chrono::Duration;
        let state = LocalState::open_memory().await.unwrap();
        let gid = Uuid::now_v7();
        let t0 = Utc::now();

        // One 90s session records duration, total, and last_played.
        state.play_start(gid, t0).await.unwrap();
        state.play_stop(gid, t0 + Duration::seconds(90)).await.unwrap();
        let s = state.play_stats().await.unwrap();
        let p = s.get(&gid).unwrap();
        assert_eq!(p.total_secs, 90);
        assert_eq!(p.last_session_secs, Some(90));
        assert!(p.last_played.is_some());

        // A second 60s session adds to the total and replaces last session.
        let t1 = t0 + Duration::seconds(3600);
        state.play_start(gid, t1).await.unwrap();
        state.play_stop(gid, t1 + Duration::seconds(60)).await.unwrap();
        let s = state.play_stats().await.unwrap();
        let p = s.get(&gid).unwrap();
        assert_eq!(p.total_secs, 150);
        assert_eq!(p.last_session_secs, Some(60));

        // Stop with no open session (daemon restarted mid-play): last_played
        // still moves, but no bogus duration is counted.
        let other = Uuid::now_v7();
        state.play_stop(other, Utc::now()).await.unwrap();
        let s = state.play_stats().await.unwrap();
        let o = s.get(&other).unwrap();
        assert_eq!(o.total_secs, 0);
        assert_eq!(o.last_session_secs, None);
        assert!(o.last_played.is_some());

        // Startup reconciliation clears an open session, so the next stop cannot
        // credit a bogus duration for a stop the daemon never actually observed.
        let g3 = Uuid::now_v7();
        state.play_start(g3, t0).await.unwrap();
        state.close_orphaned_sessions().await.unwrap();
        state.play_stop(g3, t0 + Duration::seconds(9999)).await.unwrap();
        let s = state.play_stats().await.unwrap();
        let p = s.get(&g3).unwrap();
        assert_eq!(p.total_secs, 0);
        assert_eq!(p.last_session_secs, None);
    }
}
