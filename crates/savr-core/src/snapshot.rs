//! Snapshot = the current on-disk state of one game's saves (PRD-03 §1), and
//! the diff between two snapshots that decides whether a backup is needed (§2).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::hash::Blake3Hash;
use crate::types::{FileEntry, GameId};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Snapshot {
    pub game_id: GameId,
    /// Sorted by `rel_path` for deterministic hashing/diffing.
    pub files: Vec<FileEntry>,
    pub taken_at: DateTime<Utc>,
    // Windows registry blob is captured by the daemon (PRD-02 §4); not in M0.
}

impl Snapshot {
    /// Walk resolved glob patterns into a snapshot. `anchor` is the stable
    /// save-root the `rel_path`s are computed against (so the same save set
    /// hashes identically across machines). `excludes` are glob patterns
    /// matched against each file's anchor-relative path; a matching file is
    /// omitted (e.g. `logs/**` for a manual game's noisy log directory).
    pub fn build(
        game_id: GameId,
        patterns: &[String],
        excludes: &[String],
        anchor: &Path,
    ) -> Result<Self> {
        let excludes: Vec<glob::Pattern> = excludes
            .iter()
            .filter_map(|e| glob::Pattern::new(e).ok())
            .collect();
        // BTreeMap keyed by rel_path → dedups overlapping globs and yields a
        // deterministic sort order for free.
        let mut files: BTreeMap<String, FileEntry> = BTreeMap::new();
        for pat in patterns {
            let matches = glob::glob(pat).map_err(|e| Error::Glob(e.to_string()))?;
            for path in matches.flatten() {
                collect(&path, anchor, &excludes, &mut files)?;
            }
        }
        Ok(Snapshot {
            game_id,
            files: files.into_values().collect(),
            taken_at: Utc::now(),
        })
    }
}

/// Recurse a path into `out`. A matched directory is walked; a matched file is
/// hashed. Symlinks are read as their metadata reports (not followed as dirs).
/// A file whose anchor-relative path matches one of `excludes` is skipped.
fn collect(
    path: &Path,
    anchor: &Path,
    excludes: &[glob::Pattern],
    out: &mut BTreeMap<String, FileEntry>,
) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            collect(&entry?.path(), anchor, excludes, out)?;
        }
    } else if meta.is_file() {
        let rel = rel_path(path, anchor);
        let rel_norm = rel.replace('\\', "/");
        if excludes.iter().any(|g| g.matches(&rel_norm)) {
            return Ok(());
        }
        // ponytail: whole-file read is fine for typical saves; swap to
        // Hasher::update_mmap_rayon for large (100s of MB) saves at M2.
        let bytes = std::fs::read(path)?;
        out.insert(
            rel.clone(),
            FileEntry {
                rel_path: rel,
                size: meta.len(),
                mtime: mtime_millis(&meta),
                hash: Blake3Hash::of(&bytes),
            },
        );
    }
    Ok(())
}

fn rel_path(path: &Path, anchor: &Path) -> String {
    let rel = path.strip_prefix(anchor).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn mtime_millis(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The change between two snapshots. `is_empty()` == nothing to back up
/// (PRD-03 §2: skip the upload, don't churn — G5/G6).
#[derive(Debug, Clone, Default)]
pub struct Diff {
    /// Files added or whose content hash changed.
    pub changed: Vec<FileEntry>,
    /// `rel_path`s present in `old` but gone in `new`.
    pub deleted: Vec<String>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.deleted.is_empty()
    }
}

pub fn diff(old: &Snapshot, new: &Snapshot) -> Diff {
    let old_by_path: BTreeMap<&str, &FileEntry> =
        old.files.iter().map(|f| (f.rel_path.as_str(), f)).collect();
    let new_paths: BTreeMap<&str, ()> = new
        .files
        .iter()
        .map(|f| (f.rel_path.as_str(), ()))
        .collect();

    let changed = new
        .files
        .iter()
        .filter(|f| match old_by_path.get(f.rel_path.as_str()) {
            Some(o) => o.hash != f.hash,
            None => true,
        })
        .cloned()
        .collect();

    let deleted = old
        .files
        .iter()
        .filter(|f| !new_paths.contains_key(f.rel_path.as_str()))
        .map(|f| f.rel_path.clone())
        .collect();

    Diff { changed, deleted }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn build_honors_exclude_globs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("game.sav"), b"save").unwrap();
        std::fs::create_dir(dir.path().join("logs")).unwrap();
        std::fs::write(dir.path().join("logs").join("run.log"), b"noise").unwrap();

        let pattern = format!("{}/**/*", dir.path().display());
        let snap = Snapshot::build(
            GameId::now_v7(),
            &[pattern],
            &["logs/**".to_string()],
            dir.path(),
        )
        .unwrap();

        let paths: Vec<&str> = snap.files.iter().map(|f| f.rel_path.as_str()).collect();
        assert!(paths.contains(&"game.sav"));
        assert!(
            !paths.iter().any(|p| p.contains("run.log")),
            "logs excluded"
        );
    }

    #[test]
    fn build_and_diff_detects_changes() {
        let dir = std::env::temp_dir().join(format!("savr-snap-{}", Uuid::now_v7()));
        let saves = dir.join("saves");
        fs::create_dir_all(&saves).unwrap();
        fs::write(saves.join("a.sav"), b"level 1").unwrap();
        fs::write(saves.join("b.sav"), b"gold 100").unwrap();

        let game = Uuid::nil();
        let pattern = format!("{}/**/*", saves.display());
        let s1 = Snapshot::build(game, std::slice::from_ref(&pattern), &[], &dir).unwrap();
        assert_eq!(s1.files.len(), 2);
        // rel_path is anchor-relative with forward slashes.
        assert_eq!(s1.files[0].rel_path, "saves/a.sav");

        // Identical rebuild → no diff.
        let s2 = Snapshot::build(game, std::slice::from_ref(&pattern), &[], &dir).unwrap();
        assert!(diff(&s1, &s2).is_empty(), "unchanged saves must not churn");

        // Mutate one, delete another, add a third.
        fs::write(saves.join("a.sav"), b"level 2").unwrap();
        fs::remove_file(saves.join("b.sav")).unwrap();
        fs::write(saves.join("c.sav"), b"new").unwrap();
        let s3 = Snapshot::build(game, &[pattern], &[], &dir).unwrap();
        let d = diff(&s1, &s3);
        let changed: Vec<_> = d.changed.iter().map(|f| f.rel_path.as_str()).collect();
        assert_eq!(changed, vec!["saves/a.sav", "saves/c.sav"]);
        assert_eq!(d.deleted, vec!["saves/b.sav"]);

        fs::remove_dir_all(&dir).ok();
    }
}
