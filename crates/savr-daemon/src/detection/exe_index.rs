//! The executable → game index (PRD-02 §3.2). Maps a running process's exe to
//! a `game_id`. Matching prefers an exact **canonical path** hit (an exe under
//! a known game dir) and falls back to a **basename** hit only when it is
//! unambiguous, so a generic `game.exe` shared by two titles never mismatches
//! (PRD-02 §3.1 match strategy).

use std::collections::HashMap;
use std::path::Path;

use savr_core::GameId;

/// How confident an exe→game mapping is. Path matches beat basename matches
/// (PRD-05 §3 `exe_index.confidence`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Basename = 1,
    Path = 2,
}

impl Confidence {
    pub fn as_i64(self) -> i64 {
        self as i64
    }

    pub fn from_i64(v: i64) -> Self {
        if v >= 2 {
            Confidence::Path
        } else {
            Confidence::Basename
        }
    }
}

#[derive(Debug, Default)]
pub struct ExeIndex {
    /// Canonical full path → game.
    by_path: HashMap<String, GameId>,
    /// Basename → distinct games claiming it (ambiguous if len > 1).
    by_basename: HashMap<String, Vec<GameId>>,
}

/// Normalize an exe path into a stable key. Case-insensitive filesystems
/// (Windows, macOS default) fold case so the lookup is robust.
pub fn exe_key(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        s.to_lowercase()
    } else {
        s.into_owned()
    }
}

/// Normalize just the file name into a basename key.
pub fn basename_key(path: &Path) -> Option<String> {
    path.file_name().map(|n| {
        let s = n.to_string_lossy();
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            s.to_lowercase()
        } else {
            s.into_owned()
        }
    })
}

impl ExeIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from persisted `(exe_key, game_id, confidence)` rows (PRD-05 §3).
    pub fn from_rows(rows: impl IntoIterator<Item = (String, GameId, i64)>) -> Self {
        let mut idx = ExeIndex::new();
        for (key, game_id, conf) in rows {
            match Confidence::from_i64(conf) {
                Confidence::Path => {
                    idx.by_path.insert(key, game_id);
                }
                Confidence::Basename => idx.push_basename(key, game_id),
            }
        }
        idx
    }

    fn push_basename(&mut self, key: String, game_id: GameId) {
        let games = self.by_basename.entry(key).or_default();
        if !games.contains(&game_id) {
            games.push(game_id);
        }
    }

    /// Index a full exe path for a game (path + basename entries).
    pub fn insert_exe(&mut self, exe: &Path, game_id: GameId) {
        self.by_path.insert(exe_key(exe), game_id);
        if let Some(bn) = basename_key(exe) {
            self.push_basename(bn, game_id);
        }
    }

    /// Walk a game's install dir (bounded depth) and index every executable
    /// found under it (PRD-02 §3.2 step 1).
    pub fn index_install_dir(&mut self, dir: &Path, game_id: GameId) {
        for exe in find_executables(dir, 6) {
            self.insert_exe(&exe, game_id);
        }
    }

    /// Match a running process's exe. Exact path wins; basename is a fallback
    /// only when exactly one game claims it.
    pub fn match_exe(&self, path: &Path) -> Option<(GameId, Confidence)> {
        if let Some(&g) = self.by_path.get(&exe_key(path)) {
            return Some((g, Confidence::Path));
        }
        let bn = basename_key(path)?;
        match self.by_basename.get(&bn) {
            Some(games) if games.len() == 1 => Some((games[0], Confidence::Basename)),
            // Unknown, or ambiguous basename shared by multiple games → no match.
            _ => None,
        }
    }

    /// Flatten to persistable rows (PRD-05 §3 `exe_index`).
    pub fn to_rows(&self) -> Vec<(String, GameId, i64)> {
        let mut rows = Vec::new();
        for (k, g) in &self.by_path {
            rows.push((k.clone(), *g, Confidence::Path.as_i64()));
        }
        for (k, games) in &self.by_basename {
            // Only persist unambiguous basenames; an ambiguous one is noise.
            if let [only] = games.as_slice() {
                rows.push((k.clone(), *only, Confidence::Basename.as_i64()));
            }
        }
        rows
    }

    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty() && self.by_basename.is_empty()
    }
}

/// Find executable files under `dir`, recursing up to `max_depth`. On Windows
/// that's `*.exe`; on Unix it's files with any execute bit set.
pub fn find_executables(dir: &Path, max_depth: usize) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    walk(dir, max_depth, &mut out);
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, out);
            }
        } else if is_executable(&path, &meta) {
            out.push(path);
        }
    }
}

#[cfg(unix)]
fn is_executable(_path: &Path, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(path: &Path, _meta: &std::fs::Metadata) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    #[test]
    fn path_match_beats_ambiguous_basename() {
        let hl2 = Uuid::now_v7();
        let portal = Uuid::now_v7();
        let mut idx = ExeIndex::new();

        // Two different games, each with a distinctly-pathed but same-named exe.
        idx.insert_exe(Path::new("/games/HL2/game.exe"), hl2);
        idx.insert_exe(Path::new("/games/Portal/game.exe"), portal);
        idx.insert_exe(Path::new("/games/HL2/hl2.exe"), hl2);

        // Exact path resolves unambiguously with Path confidence.
        assert_eq!(
            idx.match_exe(Path::new("/games/HL2/game.exe")),
            Some((hl2, Confidence::Path))
        );

        // A bare, ambiguous basename must NOT guess.
        assert_eq!(idx.match_exe(Path::new("/elsewhere/game.exe")), None);

        // A unique basename falls back cleanly.
        assert_eq!(
            idx.match_exe(Path::new("/elsewhere/hl2.exe")),
            Some((hl2, Confidence::Basename))
        );

        // Totally unknown exe → no match.
        assert_eq!(idx.match_exe(Path::new("/usr/bin/vim")), None);
    }

    #[test]
    fn roundtrips_through_rows() {
        let g = Uuid::now_v7();
        let mut idx = ExeIndex::new();
        idx.insert_exe(Path::new("/games/Celeste/Celeste.bin.x86_64"), g);
        let rows = idx.to_rows();
        let idx2 = ExeIndex::from_rows(rows);
        assert_eq!(
            idx2.match_exe(Path::new("/games/Celeste/Celeste.bin.x86_64")),
            Some((g, Confidence::Path))
        );
    }

    #[test]
    fn indexes_executables_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("sub").join("run");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&exe).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&exe, p).unwrap();
        }
        // A non-executable data file must be ignored on unix.
        std::fs::write(dir.path().join("data.bin"), b"x").unwrap();

        let g = Uuid::now_v7();
        let mut idx = ExeIndex::new();
        idx.index_install_dir(dir.path(), g);

        #[cfg(unix)]
        {
            assert_eq!(idx.match_exe(&exe), Some((g, Confidence::Path)));
            assert_eq!(idx.match_exe(&PathBuf::from("data.bin")).map(|_| ()), None);
        }
        // On non-unix the walk finds nothing here (no .exe); just ensure no panic.
        let _ = idx.is_empty();
    }
}
