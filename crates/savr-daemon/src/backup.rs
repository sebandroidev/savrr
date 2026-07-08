//! Backup pipeline (PRD-03 §1–3, §8): snapshot → diff → pack `.savr` → upload,
//! with an offline outbox + backoff retry when the server is unreachable.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use savr_core::archive::{self, ArchiveMeta};
use savr_core::snapshot::{self, Snapshot};
use savr_core::{
    Blake3Hash, CreateVersion, DeviceId, FileEntry, GameId, Version, VersionId, VersionKind,
};

use crate::client::{CreateOutcome, ServerClient};
use crate::state::LocalState;

/// A resolved backup request handed to the pipeline.
#[derive(Debug, Clone)]
pub struct BackupJob {
    pub game_id: GameId,
    pub patterns: Vec<String>,
    pub anchor: PathBuf,
    pub registry_keys: Vec<String>,
    pub excludes: Vec<String>,
}

/// What a backup run did.
#[derive(Debug)]
pub enum BackupOutcome {
    /// No files changed since the last snapshot — skipped (PRD-03 §2).
    NoChange,
    /// Version created and head fast-forwarded server-side.
    Uploaded { version: Version },
    /// Server unreachable / not paired — packed and queued to the outbox.
    Queued,
    /// Server stored it as a divergent branch (PRD-03 §4).
    Conflict {
        head: Option<Version>,
        incoming: Version,
    },
}

/// Serialized outbox entry (PRD-05 §3 `outbox.payload`). The archive itself is
/// kept alongside in the blob cache keyed by its hash, so retry re-uploads
/// without re-packing.
#[derive(Serialize, Deserialize)]
struct OutboxPayload {
    game_id: GameId,
    blob_hex: String,
    create_version: CreateVersion,
}

/// Run one backup for a game. `blob_cache` is where packed archives live until
/// the server confirms them (so an offline queue survives a restart).
pub async fn run_backup(
    state: &LocalState,
    client: Option<&ServerClient>,
    device_id: DeviceId,
    full_every: u32,
    job: &BackupJob,
    blob_cache: &Path,
) -> anyhow::Result<BackupOutcome> {
    // Nothing to capture — e.g. a Steam game we list and detect but whose save
    // paths aren't known yet (no manifest match, not learned). Skip rather than
    // create an empty version.
    if job.patterns.is_empty() && job.registry_keys.is_empty() {
        return Ok(BackupOutcome::NoChange);
    }
    let new_snapshot = Snapshot::build(job.game_id, &job.patterns, &job.excludes, &job.anchor)?;
    let last = state.get_snapshot(job.game_id).await?;
    let parent = last.as_ref().and_then(|s| s.local_head);

    // Diff against the last snapshot (all-changed on first backup).
    let (changed, deletions): (Vec<FileEntry>, Vec<String>) = match &last {
        Some(prev) => {
            let old = Snapshot {
                game_id: job.game_id,
                files: prev.files.clone(),
                taken_at: prev.taken_at,
            };
            let d = snapshot::diff(&old, &new_snapshot);
            if d.is_empty() && job.registry_keys.is_empty() {
                return Ok(BackupOutcome::NoChange);
            }
            (d.changed, d.deleted)
        }
        None => (new_snapshot.files.clone(), Vec::new()),
    };

    // Full vs differential (PRD-03 §2). Force a full on the first backup and
    // periodically to cap restore-chain length.
    let diff_count_key = format!("diffcount:{}", job.game_id);
    let prior_diffs: u32 = state
        .get_meta(&diff_count_key)
        .await?
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // Force a Full while the previous backup for this game is still unconfirmed
    // (queued, not yet accepted by the server). A differential's `parent` must
    // name the version whose content is its diff base; after a queue the base
    // advances locally but the server never advanced its head, so a differential
    // here would be inconsistent with any parent we could declare. A Full is
    // self-contained and safe regardless of parent.
    let unconfirmed_key = format!("unconfirmed:{}", job.game_id);
    let unconfirmed = state.get_meta(&unconfirmed_key).await?.as_deref() == Some("1");
    let kind = if parent.is_none() || prior_diffs >= full_every || unconfirmed {
        VersionKind::Full
    } else {
        VersionKind::Differential
    };

    // Payload = all files for a full, only changed files for a differential.
    let payload_files: Vec<FileEntry> = match kind {
        VersionKind::Full => new_snapshot.files.clone(),
        VersionKind::Differential => changed,
    };
    let deletions = match kind {
        VersionKind::Full => Vec::new(),
        VersionKind::Differential => deletions,
    };

    let registry = crate::paths::capture_registry(&job.registry_keys)?;

    let meta = ArchiveMeta {
        game_id: job.game_id,
        device_id,
        parent,
        kind,
        files: new_snapshot.files.clone(),
        deletions,
        created_at: Utc::now(),
    };

    // Pack into the blob cache under a temp name, then rename to its content
    // hash once known.
    std::fs::create_dir_all(blob_cache)?;
    let tmp_path = blob_cache.join(format!("packing-{}.savr", uuid::Uuid::now_v7()));
    let payload = build_payload(&payload_files, &job.anchor);
    let (blob_hash, bytes) = pack_to_file(&meta, &payload, registry.as_deref(), &tmp_path)?;
    let final_path = blob_cache.join(format!("{}.savr", blob_hash.to_hex()));
    std::fs::rename(&tmp_path, &final_path)?;

    let create = CreateVersion {
        parent,
        kind,
        files: new_snapshot.files.clone(),
        blob_hash,
        bytes,
        device_id,
    };

    // The diff base (used for the next diff AND for NoChange detection) is only
    // advanced once this change is durable — server-confirmed, or safely in the
    // outbox. Advancing it before durability could lose the change on a crash;
    // and on the confirmed paths the base's local_head names a version whose
    // content IS these files, keeping later differentials consistent.
    let files = new_snapshot.files;
    let taken_at = new_snapshot.taken_at;

    let authed = match client {
        Some(c) => c.is_authenticated().await,
        None => false,
    };

    if let (Some(client), true) = (client, authed) {
        match upload_version(client, job.game_id, &create, &final_path).await {
            Ok(CreateOutcome::Created(version)) => {
                // Confirmed: base == version.id's content; differentials resume.
                state
                    .put_snapshot(job.game_id, &files, taken_at, Some(version.id))
                    .await?;
                state.set_meta(&unconfirmed_key, "0").await?;
                bump_diff_counter(state, &diff_count_key, kind, prior_diffs).await?;
                state.set_last_backup_at(Utc::now()).await?;
                let _ = std::fs::remove_file(&final_path);
                Ok(BackupOutcome::Uploaded { version })
            }
            Ok(CreateOutcome::Conflict { head, incoming }) => {
                // The branch is stored server-side; track our tip locally so the
                // next diff is against what we just backed up (PRD-03 §4).
                state
                    .put_snapshot(job.game_id, &files, taken_at, Some(incoming.id))
                    .await?;
                state.set_meta(&unconfirmed_key, "0").await?;
                let _ = std::fs::remove_file(&final_path);
                Ok(BackupOutcome::Conflict { head, incoming })
            }
            Err(e) => {
                tracing::warn!("upload failed, queueing to outbox: {e}");
                queue_backup(
                    state,
                    job.game_id,
                    &create,
                    &blob_hash,
                    &files,
                    taken_at,
                    parent,
                    &unconfirmed_key,
                )
                .await?;
                Ok(BackupOutcome::Queued)
            }
        }
    } else {
        queue_backup(
            state,
            job.game_id,
            &create,
            &blob_hash,
            &files,
            taken_at,
            parent,
            &unconfirmed_key,
        )
        .await?;
        Ok(BackupOutcome::Queued)
    }
}

/// Queue a packed backup durably, THEN advance the local diff-base. The order
/// matters: if the process dies between the two, the archive is already in the
/// outbox (nothing lost) and the base simply isn't advanced yet, so the next
/// run re-detects and re-queues the change. The base keeps the OLD `local_head`
/// (this version is not server-confirmed), and the game is flagged
/// `unconfirmed` so the next backup is a self-contained Full rather than a
/// differential whose parent the server may never advance to.
#[allow(clippy::too_many_arguments)]
async fn queue_backup(
    state: &LocalState,
    game_id: GameId,
    create: &CreateVersion,
    blob_hash: &Blake3Hash,
    files: &[FileEntry],
    taken_at: chrono::DateTime<Utc>,
    parent: Option<VersionId>,
    unconfirmed_key: &str,
) -> anyhow::Result<()> {
    enqueue(state, game_id, create, blob_hash).await?;
    state.put_snapshot(game_id, files, taken_at, parent).await?;
    state.set_meta(unconfirmed_key, "1").await?;
    Ok(())
}

async fn bump_diff_counter(
    state: &LocalState,
    key: &str,
    kind: VersionKind,
    prior: u32,
) -> anyhow::Result<()> {
    let next = match kind {
        VersionKind::Full => 0,
        VersionKind::Differential => prior + 1,
    };
    state.set_meta(key, &next.to_string()).await
}

async fn enqueue(
    state: &LocalState,
    game_id: GameId,
    create: &CreateVersion,
    blob_hash: &Blake3Hash,
) -> anyhow::Result<()> {
    let payload = OutboxPayload {
        game_id,
        blob_hex: blob_hash.to_hex(),
        create_version: create.clone(),
    };
    let bytes = serde_json::to_vec(&payload)?;
    // Key the outbox by a client-side id; the server mints the real version id.
    state
        .enqueue_outbox(uuid::Uuid::now_v7(), &bytes, Utc::now())
        .await?;
    Ok(())
}

/// HEAD the blob (dedup), PUT if absent, then POST the version (PRD-04 §2).
async fn upload_version(
    client: &ServerClient,
    game_id: GameId,
    create: &CreateVersion,
    archive_path: &Path,
) -> anyhow::Result<CreateOutcome> {
    if !client.blob_exists(&create.blob_hash).await? {
        client
            .blob_put_file(&create.blob_hash, archive_path)
            .await?;
    }
    client.create_version(game_id, create).await
}

/// Drain due outbox entries, retrying uploads with exponential backoff
/// (PRD-03 §8).
pub async fn retry_outbox(
    state: &LocalState,
    client: &ServerClient,
    blob_cache: &Path,
) -> anyhow::Result<()> {
    if !client.is_authenticated().await {
        return Ok(());
    }
    let now = Utc::now();
    for item in state.due_outbox(now).await? {
        let payload: OutboxPayload = match serde_json::from_slice(&item.payload) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("dropping unparseable outbox entry {}: {e}", item.version_id);
                state.remove_outbox(item.version_id).await?;
                continue;
            }
        };
        let archive_path = blob_cache.join(format!("{}.savr", payload.blob_hex));
        match upload_version(
            client,
            payload.game_id,
            &payload.create_version,
            &archive_path,
        )
        .await
        {
            Ok(CreateOutcome::Created(version)) => {
                state.set_local_head(payload.game_id, version.id).await?;
                state.set_last_backup_at(Utc::now()).await?;
                state.remove_outbox(item.version_id).await?;
                let _ = std::fs::remove_file(&archive_path);
            }
            Ok(CreateOutcome::Conflict { incoming, .. }) => {
                state.set_local_head(payload.game_id, incoming.id).await?;
                state.remove_outbox(item.version_id).await?;
                let _ = std::fs::remove_file(&archive_path);
            }
            Err(e) => {
                let delay = backoff_delay(item.attempts as u32);
                tracing::warn!(
                    "outbox retry for {} failed (attempt {}): {e}; next in {:?}",
                    item.version_id,
                    item.attempts,
                    delay
                );
                let next =
                    now + ChronoDuration::from_std(delay).unwrap_or(ChronoDuration::seconds(60));
                state.bump_outbox_attempt(item.version_id, next).await?;
            }
        }
    }
    Ok(())
}

/// Exponential backoff with a cap (PRD-03 §8): 1s, 2s, 4s, 8s … capped at 5 min.
pub fn backoff_delay(attempts: u32) -> Duration {
    let secs = 2u64.saturating_pow(attempts.min(9));
    Duration::from_secs(secs.min(300))
}

/// Map `FileEntry`s to `(rel_path, absolute source path)` pairs for `pack`.
pub fn build_payload(files: &[FileEntry], anchor: &Path) -> Vec<(String, PathBuf)> {
    files
        .iter()
        .map(|f| (f.rel_path.clone(), anchor.join(&f.rel_path)))
        .collect()
}

/// Pack an archive to `out_path` and return its content hash + byte length.
///
/// ponytail: hashes by reading the finished file back (fine for small saves);
/// hash-while-writing for multi-hundred-MB archives at M2.
pub fn pack_to_file(
    meta: &ArchiveMeta,
    payload: &[(String, PathBuf)],
    registry: Option<&[u8]>,
    out_path: &Path,
) -> anyhow::Result<(Blake3Hash, u64)> {
    let file = std::fs::File::create(out_path)?;
    archive::pack(file, meta, payload, registry)?;
    let bytes = std::fs::read(out_path)?;
    Ok((Blake3Hash::of(&bytes), bytes.len() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use savr_core::archive::unpack;
    use uuid::Uuid;

    // The core pipeline: create files → snapshot → diff → pack → unpack → bytes
    // match. Exercises core end-to-end with no server (PRD-03 §1–3).
    #[test]
    fn backup_roundtrip_bytes_match() {
        let work = tempfile::tempdir().unwrap();
        let saves = work.path().join("saves");
        std::fs::create_dir_all(saves.join("sub")).unwrap();
        std::fs::write(saves.join("a.sav"), b"level 1 progress").unwrap();
        std::fs::write(saves.join("sub/b.sav"), b"gold: 9999").unwrap();

        let game = Uuid::now_v7();
        let device = Uuid::now_v7();
        let anchor = work.path().to_path_buf();
        let pattern = format!("{}/**/*", saves.display());

        // Snapshot.
        let snap = Snapshot::build(game, std::slice::from_ref(&pattern), &[], &anchor).unwrap();
        assert_eq!(snap.files.len(), 2);

        // Diff vs empty → everything changed (a full).
        let empty = Snapshot {
            game_id: game,
            files: vec![],
            taken_at: snap.taken_at,
        };
        let d = snapshot::diff(&empty, &snap);
        assert_eq!(d.changed.len(), 2);
        assert!(d.deleted.is_empty());

        // Pack.
        let meta = ArchiveMeta {
            game_id: game,
            device_id: device,
            parent: None,
            kind: VersionKind::Full,
            files: snap.files.clone(),
            deletions: vec![],
            created_at: Utc::now(),
        };
        let payload = build_payload(&snap.files, &anchor);
        let out = work.path().join("v.savr");
        let (hash, bytes) = pack_to_file(&meta, &payload, None, &out).unwrap();
        assert!(bytes > 0);
        // Hash is stable / content-addressed.
        assert_eq!(hash, Blake3Hash::of(&std::fs::read(&out).unwrap()));

        // Unpack into a fresh dir and compare bytes.
        let restore = work.path().join("restore");
        let unpacked = unpack(std::fs::File::open(&out).unwrap(), &restore).unwrap();
        assert_eq!(unpacked.meta.files.len(), 2);
        assert_eq!(
            std::fs::read(restore.join("saves/a.sav")).unwrap(),
            b"level 1 progress"
        );
        assert_eq!(
            std::fs::read(restore.join("saves/sub/b.sav")).unwrap(),
            b"gold: 9999"
        );
    }

    #[tokio::test]
    async fn no_change_is_skipped() {
        let work = tempfile::tempdir().unwrap();
        let saves = work.path().join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("a.sav"), b"x").unwrap();

        let state = LocalState::open_memory().await.unwrap();
        let game = Uuid::now_v7();
        let device = Uuid::now_v7();
        let job = BackupJob {
            game_id: game,
            patterns: vec![format!("{}/**/*", saves.display())],
            anchor: work.path().to_path_buf(),
            registry_keys: vec![],
            excludes: vec![],
        };
        let cache = work.path().join("cache");

        // First backup with no server → queued.
        let out = run_backup(&state, None, device, 10, &job, &cache)
            .await
            .unwrap();
        assert!(matches!(out, BackupOutcome::Queued));

        // Second, unchanged → skipped.
        let out = run_backup(&state, None, device, 10, &job, &cache)
            .await
            .unwrap();
        assert!(matches!(out, BackupOutcome::NoChange));
    }

    // Regression: after a backup is queued offline, the NEXT changed backup
    // must be a self-contained Full, not a differential that declares a parent
    // (the stale confirmed head) inconsistent with its advanced diff base.
    #[tokio::test]
    async fn queued_then_changed_forces_full() -> anyhow::Result<()> {
        let work = tempfile::tempdir().unwrap();
        let saves = work.path().join("saves");
        std::fs::create_dir_all(&saves).unwrap();
        std::fs::write(saves.join("a.sav"), b"v1").unwrap();

        let state = LocalState::open_memory().await.unwrap();
        let game = Uuid::now_v7();
        let device = Uuid::now_v7();
        let anchor = work.path().to_path_buf();
        let job = BackupJob {
            game_id: game,
            patterns: vec![format!("{}/**/*", saves.display())],
            anchor: anchor.clone(),
            registry_keys: vec![],
            excludes: vec![],
        };
        let cache = work.path().join("cache");

        // Simulate a prior CONFIRMED backup at head_a whose content is v1, so the
        // first offline backup below is a legitimate differential from head_a.
        let head_a = Uuid::now_v7();
        let base = Snapshot::build(game, &job.patterns, &job.excludes, &anchor).unwrap();
        state
            .put_snapshot(game, &base.files, base.taken_at, Some(head_a))
            .await?;

        // First offline change -> queued as a differential (parent = head_a).
        std::fs::write(saves.join("a.sav"), b"v2").unwrap();
        assert!(matches!(
            run_backup(&state, None, device, 10, &job, &cache).await?,
            BackupOutcome::Queued
        ));

        // Second offline change -> queued, and MUST be forced Full.
        std::fs::write(saves.join("a.sav"), b"v3").unwrap();
        assert!(matches!(
            run_backup(&state, None, device, 10, &job, &cache).await?,
            BackupOutcome::Queued
        ));

        // Inspect the outbox: exactly one Full and one Differential.
        let items = state
            .due_outbox(Utc::now() + ChronoDuration::seconds(1))
            .await?;
        let kinds: Vec<VersionKind> = items
            .iter()
            .map(|it| {
                serde_json::from_slice::<OutboxPayload>(&it.payload)
                    .unwrap()
                    .create_version
                    .kind
            })
            .collect();
        let fulls = kinds
            .iter()
            .filter(|k| matches!(k, VersionKind::Full))
            .count();
        assert_eq!(items.len(), 2, "both backups should be queued");
        assert_eq!(fulls, 1, "the second (post-queue) backup must be a Full");
        Ok(())
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(20), Duration::from_secs(300));
    }
}
