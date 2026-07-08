//! Restore pipeline (PRD-03 §6): safe by construction. Refuse while the game
//! runs, take a pre-restore backup so it's undoable, download the target
//! version plus its differential chain back to the last full, verify archive
//! integrity, then swap the reconstructed files into place atomically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use savr_core::archive::unpack;
use savr_core::{Blake3Hash, DeviceId, GameId, Version, VersionId, VersionKind};

use crate::backup::{run_backup, BackupJob};
use crate::client::ServerClient;
use crate::state::LocalState;

/// A resolved restore request.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    pub game_id: GameId,
    pub version_id: VersionId,
    pub anchor: PathBuf,
    /// Save globs (used for the pre-restore backup + local deletion sync).
    pub patterns: Vec<String>,
    pub registry_keys: Vec<String>,
}

/// Restore `req.version_id` for a game.
///
/// `game_running` must be checked by the caller against the detection engine —
/// restoring a live game corrupts the save (PRD-03 §6 step 1).
pub async fn run_restore(
    state: &LocalState,
    client: &ServerClient,
    device_id: DeviceId,
    req: &RestoreRequest,
    game_running: bool,
    blob_cache: &Path,
    scratch: &Path,
) -> anyhow::Result<()> {
    if game_running {
        anyhow::bail!("refusing to restore {}: game is running", req.game_id);
    }

    // No save targets means the anchor defaulted to $HOME (paths.rs): restoring
    // would write and delete files directly under the home directory. Refuse.
    if req.patterns.is_empty() && req.registry_keys.is_empty() {
        anyhow::bail!(
            "refusing to restore {}: no known save paths for this game",
            req.game_id
        );
    }

    // 1. Pre-restore backup so the restore is undoable (PRD-03 §6 step 2).
    let pre = BackupJob {
        game_id: req.game_id,
        patterns: req.patterns.clone(),
        anchor: req.anchor.clone(),
        registry_keys: req.registry_keys.clone(),
        // RestoreRequest carries no excludes (out of scope for Task 4): a
        // pre-restore backup captures everything under the resolved patterns.
        excludes: Vec::new(),
    };
    if let Err(e) = run_backup(state, Some(client), device_id, u32::MAX, &pre, blob_cache).await {
        // A failed pre-backup (e.g. offline) must not silently proceed to a
        // destructive restore.
        anyhow::bail!("pre-restore backup failed, aborting restore: {e}");
    }

    // 2. Build the version chain: target back to the last full (PRD-03 §6.3).
    let versions = client.list_versions(req.game_id).await?;
    let chain = build_chain(&versions, req.version_id)?;

    // 3. Download + verify + unpack each archive, oldest (full) first, merging
    // changed files and honoring deletions.
    std::fs::create_dir_all(scratch)?;
    let mut merged: HashMap<String, PathBuf> = HashMap::new();
    for version in &chain {
        let staged = scratch.join(version.id.to_string());
        let archive_path = scratch.join(format!("{}.savr", version.blob_hash.to_hex()));
        client
            .blob_get_to_file(&version.blob_hash, &archive_path)
            .await?;
        verify_blob(&archive_path, &version.blob_hash)?;

        let unpacked = unpack(std::fs::File::open(&archive_path)?, &staged)?;
        // Physically-present (changed) files override; deletions drop entries.
        for rel in walk_rel(&staged) {
            merged.insert(rel.clone(), staged.join(&rel));
        }
        for del in &unpacked.meta.deletions {
            merged.remove(del);
        }
        // ponytail: registry bytes from the final version are unpacked but not
        // written back — HKCU import is the Windows-milestone stub (paths.rs).
        let _ = unpacked.registry;
    }

    // 4. Swap into place atomically: write each file via temp + rename.
    for (rel, src) in &merged {
        let dest = req.anchor.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = dest.with_extension("savr-restore-tmp");
        std::fs::copy(src, &tmp)?;
        std::fs::rename(&tmp, &dest)?;
    }

    // 5. Delete local files the target version no longer has (sync deletions),
    // relative to our last snapshot so we never touch unrelated files.
    if let Some(last) = state.get_snapshot(req.game_id).await? {
        for f in &last.files {
            if !merged.contains_key(&f.rel_path) {
                let _ = std::fs::remove_file(req.anchor.join(&f.rel_path));
            }
        }
    }

    // 6. Update local head + snapshot to the restored version (PRD-03 §6.6).
    let target = chain
        .last()
        .ok_or_else(|| anyhow::anyhow!("empty restore chain"))?;
    state
        .put_snapshot(
            req.game_id,
            &target.files,
            target.created_at,
            Some(target.id),
        )
        .await?;

    Ok(())
}

/// Verify an archive on disk matches its declared content hash (PRD-03 §8:
/// corrupt archive → abort, never write garbage over good saves).
fn verify_blob(path: &Path, expected: &Blake3Hash) -> anyhow::Result<()> {
    let bytes = std::fs::read(path)?;
    let got = Blake3Hash::of(&bytes);
    if &got != expected {
        anyhow::bail!(
            "archive integrity check failed: got {}, expected {}",
            got.to_hex(),
            expected.to_hex()
        );
    }
    Ok(())
}

/// Build the apply-order chain [full, …, target] by walking `parent` pointers
/// until a `Full` is reached.
fn build_chain(versions: &[Version], target: VersionId) -> anyhow::Result<Vec<Version>> {
    let by_id: HashMap<VersionId, &Version> = versions.iter().map(|v| (v.id, v)).collect();
    let mut chain = Vec::new();
    let mut cursor = Some(target);
    while let Some(id) = cursor {
        let v = by_id
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("version {id} missing from history"))?;
        chain.push((*v).clone());
        if v.kind == VersionKind::Full {
            break;
        }
        cursor = v.parent;
    }
    if chain.last().map(|v| v.kind) != Some(VersionKind::Full) {
        anyhow::bail!("differential chain for {target} never reaches a full backup");
    }
    chain.reverse(); // full first, target last
    Ok(chain)
}

/// All file paths under `dir`, relative to `dir`, with forward slashes.
fn walk_rel(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk_rel_inner(dir, dir, &mut out);
    out
}

fn walk_rel_inner(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rel_inner(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn version(id: VersionId, parent: Option<VersionId>, kind: VersionKind) -> Version {
        Version {
            id,
            game_id: Uuid::nil(),
            device_id: Uuid::nil(),
            parent,
            kind,
            files: vec![],
            blob_hash: Blake3Hash::of(b""),
            bytes: 0,
            seq: 0,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn chain_from_diff_back_to_full() {
        let full = Uuid::now_v7();
        let d1 = Uuid::now_v7();
        let d2 = Uuid::now_v7();
        let versions = vec![
            version(full, None, VersionKind::Full),
            version(d1, Some(full), VersionKind::Differential),
            version(d2, Some(d1), VersionKind::Differential),
        ];
        let chain = build_chain(&versions, d2).unwrap();
        let ids: Vec<_> = chain.iter().map(|v| v.id).collect();
        assert_eq!(ids, vec![full, d1, d2], "full first, target last");
    }

    #[test]
    fn chain_without_full_errors() {
        let d1 = Uuid::now_v7();
        // A differential whose parent is missing / never a full.
        let versions = vec![version(d1, None, VersionKind::Differential)];
        assert!(build_chain(&versions, d1).is_err());
    }
}
