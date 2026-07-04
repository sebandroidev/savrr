//! Retention GC (PRD-03 §7): with default retention (keep 5 fulls), a game with
//! 8 full versions is pruned down to 5, the current head is never pruned, and
//! the orphaned blobs are deleted (refcount -> 0).

mod common;

use common::setup;
use savr_server::{db, FsBlobStore};
use uuid::Uuid;

#[tokio::test]
async fn gc_prunes_old_fulls_and_keeps_head() {
    let ctx = setup().await;
    let pool = ctx.pool.clone();
    let account = ctx.account;
    let device = ctx.device;

    // A game with a linear chain of 8 full versions, each with its own blob.
    let game = Uuid::now_v7();
    sqlx::query("INSERT INTO games (id, account_id, title) VALUES (?, ?, 'GC Game')")
        .bind(game.to_string())
        .bind(account.to_string())
        .execute(&pool)
        .await
        .unwrap();

    let mut ids = Vec::new();
    let mut parent: Option<String> = None;
    for i in 1..=8u32 {
        let vid = Uuid::now_v7().to_string();
        let hash = blake3::hash(format!("blob-{i}").as_bytes())
            .to_hex()
            .to_string();
        sqlx::query("INSERT INTO blobs (hash, bytes, refcount, created_at) VALUES (?, 10, 1, '2026-01-01T00:00:00Z')")
            .bind(&hash)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO versions
             (id, game_id, account_id, device_id, parent, kind, blob_hash, files_json, bytes, seq, created_at)
             VALUES (?, ?, ?, ?, ?, 'full', ?, '[]', 10, ?, '2026-01-01T00:00:00Z')",
        )
        .bind(&vid)
        .bind(game.to_string())
        .bind(account.to_string())
        .bind(device.to_string())
        .bind(&parent)
        .bind(&hash)
        .bind(i as i64)
        .execute(&pool)
        .await
        .unwrap();
        parent = Some(vid.clone());
        ids.push(vid);
    }
    // Head = newest version (v8).
    let head_id = ids.last().unwrap().clone();
    sqlx::query("UPDATE games SET head = ? WHERE id = ?")
        .bind(&head_id)
        .bind(game.to_string())
        .execute(&pool)
        .await
        .unwrap();

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM versions WHERE game_id = ?")
        .bind(game.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, 8);

    // Run GC (default retention = 5 fulls; no config row -> default).
    let blobs = FsBlobStore::new(std::env::temp_dir().join(format!("savr-gc-{}", Uuid::now_v7())));
    let stats = db::run_gc(&pool, &blobs).await.unwrap();
    assert_eq!(stats.versions_pruned, 3, "8 fulls, keep 5 -> prune 3");
    assert_eq!(
        stats.blobs_deleted, 3,
        "each pruned version's blob is orphaned"
    );

    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM versions WHERE game_id = ?")
        .bind(game.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, 5, "version count drops to the retention limit");

    // Head must survive.
    let head_alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM versions WHERE id = ?")
        .bind(&head_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(head_alive, 1, "current head is never pruned");

    // The three oldest were the ones pruned.
    for old in ids.iter().take(3) {
        let alive: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM versions WHERE id = ?")
            .bind(old)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(alive, 0, "oldest fulls are pruned");
    }

    // Orphaned blob rows are gone too.
    let blob_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(blob_rows, 5, "deref'd blobs are deleted");
}
