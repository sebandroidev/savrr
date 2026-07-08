# Non-Steam Games (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let Savr back up games not installed through Steam — by auto-detecting manifest-known games from their install-folder name (Ludusavi-style) and by letting the user add unknown games manually with a save folder + globs.

**Architecture:** A new persisted `custom_games` table plus a folder→manifest name matcher feed extra entries into the existing catalog. `Engine::refresh_games` gains two new sources after its Steam scan (auto-detected folder-root games, then manual games); both merge into the one in-memory catalog and the one rebuilt `ExeIndex`, so detection and the snapshot→diff→upload backup pipeline downstream are unchanged. Cross-device identity for non-Steam games is by canonical/normalized name.

**Tech Stack:** Rust (tokio, sqlx/SQLite, glob, serde), Tauri v2 command layer, Svelte UI, `@tauri-apps/plugin-dialog` for folder/file pickers.

## Global Constraints

- Workspace version currently `0.1.9`; this feature ships as a later release (do not bump version inside these tasks).
- `GuiRequest` is internally tagged (`#[serde(tag = "type")]`): new variants MUST be **struct variants** (`Variant { field: T }`), never newtype-wrapping a primitive or `Vec`. Struct variants with `Vec`/`Option` *fields* are fine. There is a round-trip test pattern in `crates/savr-core/src/ipc.rs` (`shutdown_encodes`, `set_autostart_encodes`) — extend it for each new variant.
- `ipc.rs` tests are gated behind `#[cfg(all(test, feature = "ipc"))]` — run core ipc tests with `cargo test -p savr-core --lib --features ipc`.
- Commit footer on every commit:
  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_0192RvHF58xWY7w3EkDiYiH6
  ```
- Run `cargo fmt --all` before every commit; keep `cargo clippy --workspace --all-targets` clean.
- `game_id_for` is infallible by design — never let a server/DB hiccup fail a catalog refresh.
- Never guess a manifest match: an ambiguous (>1 game) or absent folder-name match is skipped, not resolved to a best-guess.

---

## File Structure

**New files:**
- `crates/savr-daemon/src/naming.rs` — `normalize_title(&str) -> String`. Shared by folder matching and name-based identity.
- `crates/savr-daemon/src/scan.rs` — `ManifestMatcher` (folder-name → canonical manifest title) and `scan_folder_root` (list a root's subfolders and match each).

**Modified files:**
- `crates/savr-core/src/snapshot.rs` — `Snapshot::build` gains an `excludes` parameter.
- `crates/savr-daemon/src/lib.rs` — declare `mod naming; mod scan;`.
- `crates/savr-daemon/src/state.rs` — `custom_games` table + CRUD; `CustomGame` struct.
- `crates/savr-daemon/src/paths.rs` — `ResolvedGame.excludes`; `resolve_game` gains a `base_override`; new `resolve_custom`.
- `crates/savr-daemon/src/backup.rs` — `BackupJob.excludes`; pass through to `Snapshot::build`.
- `crates/savr-daemon/src/engine.rs` — `backup_job` copies excludes; `game_id_for` takes `Option<u32>`; `refresh_games` adds the two new sources; handle `AddCustomGame`/`RemoveCustomGame`.
- `crates/savr-core/src/ipc.rs` — `GuiRequest::AddCustomGame`/`RemoveCustomGame` + `CustomGameSpec` struct + encode tests.
- `crates/savr-app/src-tauri/src/commands.rs` + `lib.rs` — `add_custom_game`, `remove_custom_game` commands.
- `crates/savr-app/ui/src/lib/api.ts`, `types.ts` — wrappers + types.
- `crates/savr-app/ui/src/views/Games.svelte` (+ a small `AddGameDialog.svelte`) — UI.

---

## Task 1: Title normalization

**Files:**
- Create: `crates/savr-daemon/src/naming.rs`
- Modify: `crates/savr-daemon/src/lib.rs` (add `mod naming;`)
- Test: in `crates/savr-daemon/src/naming.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `pub fn normalize_title(s: &str) -> String` — lowercased, with every character that is not ASCII alphanumeric removed. `"Hollow Knight"`, `"HollowKnight"`, and `"hollow-knight!"` all normalize to `"hollowknight"`.

- [ ] **Step 1: Write the failing test**

Create `crates/savr-daemon/src/naming.rs` with only the test module:

```rust
//! Title normalization shared by folder→manifest matching (`scan`) and
//! name-based cross-device identity (`engine::game_id_for`).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_case_space_and_punctuation() {
        assert_eq!(normalize_title("Hollow Knight"), "hollowknight");
        assert_eq!(normalize_title("HollowKnight"), "hollowknight");
        assert_eq!(normalize_title("  hollow-knight! "), "hollowknight");
        assert_eq!(normalize_title("Baldur's Gate 3"), "baldursgate3");
    }

    #[test]
    fn empty_when_no_alphanumerics() {
        assert_eq!(normalize_title("!!!"), "");
    }
}
```

Add `mod naming;` to `crates/savr-daemon/src/lib.rs` (near the other `mod` lines).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savr-daemon naming`
Expected: FAIL — `cannot find function 'normalize_title'`.

- [ ] **Step 3: Write minimal implementation**

Add above the test module in `naming.rs`:

```rust
/// Lowercase and strip every non-ASCII-alphanumeric character, so folder names,
/// manifest `installDir` keys, and titles compare regardless of spacing, case,
/// and punctuation.
pub fn normalize_title(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p savr-daemon naming`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/savr-daemon/src/naming.rs crates/savr-daemon/src/lib.rs
git commit -m "feat(daemon): title normalization for name matching"
```

---

## Task 2: Folder → manifest matcher

**Files:**
- Create: `crates/savr-daemon/src/scan.rs`
- Modify: `crates/savr-daemon/src/lib.rs` (add `mod scan;`)
- Test: in `crates/savr-daemon/src/scan.rs`

**Interfaces:**
- Consumes: `crate::naming::normalize_title`; `savr_core::manifest::Manifest` (iterable as `(&String, &ManifestEntry)` via `.iter()`); `ManifestEntry::install_dir: BTreeMap<String, IgnoredAny>` (keys are folder names).
- Produces:
  - `pub struct ManifestMatcher` with `pub fn build(manifest: &Manifest) -> Self` and `pub fn match_folder(&self, folder_name: &str) -> Option<String>` returning the **canonical manifest title** for a uniquely-matched folder, `None` when unknown or ambiguous.
  - `pub fn scan_folder_root(root: &std::path::Path) -> Vec<(String, std::path::PathBuf)>` returning `(folder_name, absolute_path)` for each immediate subdirectory (one level deep).

- [ ] **Step 1: Write the failing test**

Create `crates/savr-daemon/src/scan.rs`:

```rust
//! Ludusavi-style detection: match a game's install-folder name against the
//! Ludusavi manifest (its `installDir` keys and title), and enumerate the
//! install dirs under a generic "game folder" root. One level deep only.

use std::path::{Path, PathBuf};

use savr_core::manifest::Manifest;

use crate::naming::normalize_title;

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_from(yaml: &str) -> Manifest {
        savr_core::manifest::parse(yaml).unwrap()
    }

    #[test]
    fn matches_by_title_and_install_dir_key() {
        let m = manifest_from(
            "\
Hollow Knight:
  files:
    <base>/saves: {}
  installDir:
    Hollow Knight: {}
Celeste:
  files:
    <base>/Saves: {}
",
        );
        let matcher = ManifestMatcher::build(&m);
        // Exact title, spacing/case variants, and the installDir key all match.
        assert_eq!(matcher.match_folder("Hollow Knight").as_deref(), Some("Hollow Knight"));
        assert_eq!(matcher.match_folder("hollowknight").as_deref(), Some("Hollow Knight"));
        assert_eq!(matcher.match_folder("Celeste").as_deref(), Some("Celeste"));
        assert_eq!(matcher.match_folder("Some Random Folder"), None);
    }

    #[test]
    fn ambiguous_normalized_name_is_skipped() {
        // Two different games whose normalized names collide → never guess.
        let m = manifest_from(
            "\
Game X:
  files:
    <base>/a: {}
GameX:
  files:
    <base>/b: {}
",
        );
        let matcher = ManifestMatcher::build(&m);
        assert_eq!(matcher.match_folder("gamex"), None);
    }

    #[test]
    fn scan_lists_immediate_subdirs_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Hollow Knight")).unwrap();
        std::fs::create_dir(dir.path().join("Celeste")).unwrap();
        std::fs::create_dir_all(dir.path().join("Celeste").join("nested")).unwrap();
        std::fs::write(dir.path().join("loose.txt"), b"x").unwrap();

        let mut names: Vec<String> =
            scan_folder_root(dir.path()).into_iter().map(|(n, _)| n).collect();
        names.sort();
        assert_eq!(names, vec!["Celeste".to_string(), "Hollow Knight".to_string()]);
    }
}
```

Add `mod scan;` to `crates/savr-daemon/src/lib.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savr-daemon scan`
Expected: FAIL — `ManifestMatcher`/`scan_folder_root` not found.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module:

```rust
use std::collections::HashMap;

/// A normalized-name → canonical-title index over the manifest. Names that map
/// to more than one distinct title are recorded as ambiguous and never match.
pub struct ManifestMatcher {
    index: HashMap<String, Match>,
}

enum Match {
    One(String),
    Ambiguous,
}

impl ManifestMatcher {
    pub fn build(manifest: &Manifest) -> Self {
        let mut index: HashMap<String, Match> = HashMap::new();
        let mut add = |key: String, title: &str| {
            if key.is_empty() {
                return;
            }
            match index.get_mut(&key) {
                None => {
                    index.insert(key, Match::One(title.to_string()));
                }
                Some(Match::One(existing)) if existing != title => {
                    index.insert(key, Match::Ambiguous);
                }
                _ => {}
            }
        };
        for (title, entry) in manifest.iter() {
            add(normalize_title(title), title);
            for install_dir in entry.install_dir.keys() {
                add(normalize_title(install_dir), title);
            }
        }
        Self { index }
    }

    /// The canonical manifest title for a uniquely-matched folder name, or
    /// `None` when unknown or ambiguous.
    pub fn match_folder(&self, folder_name: &str) -> Option<String> {
        match self.index.get(&normalize_title(folder_name)) {
            Some(Match::One(title)) => Some(title.clone()),
            _ => None,
        }
    }
}

/// Immediate subdirectories of a game-folder root, as `(name, absolute_path)`.
/// Unreadable roots yield an empty list (logged by the caller).
pub fn scan_folder_root(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                out.push((name.to_string(), path));
            }
        }
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p savr-daemon scan`
Expected: PASS (3 tests).

Note: if `manifest::parse` is not `pub`, use the same construction the existing `manifest` tests use — check `crates/savr-core/src/manifest.rs` test module; `parse` is `pub fn parse(yaml: &str) -> Result<Manifest>` (line ~80), so `savr_core::manifest::parse` works.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/savr-daemon/src/scan.rs crates/savr-daemon/src/lib.rs
git commit -m "feat(daemon): match install-folder names to the manifest"
```

---

## Task 3: `custom_games` table + state CRUD

**Files:**
- Modify: `crates/savr-daemon/src/state.rs`
- Test: `crates/savr-daemon/src/state.rs` (`#[cfg(test)]`, alongside `roots_and_meta_roundtrip`)

**Interfaces:**
- Produces on `LocalState`:
  - `pub async fn add_custom_game(&self, g: &CustomGame) -> anyhow::Result<()>` — errors if the normalized title already exists.
  - `pub async fn list_custom_games(&self) -> anyhow::Result<Vec<CustomGame>>`
  - `pub async fn remove_custom_game(&self, norm_title: &str) -> anyhow::Result<()>`
  - `pub struct CustomGame { pub title: String, pub install_path: Option<String>, pub save_root: String, pub include: Vec<String>, pub exclude: Vec<String> }`
  - `pub fn norm_key(title: &str) -> String` is `crate::naming::normalize_title`; state stores it as the primary key.

- [ ] **Step 1: Write the failing test**

Add to the `state.rs` test module:

```rust
#[tokio::test]
async fn custom_games_roundtrip() {
    let state = LocalState::open_memory().await.unwrap();
    let g = CustomGame {
        title: "My Cracked Game".to_string(),
        install_path: Some("D:/Games/MyGame".to_string()),
        save_root: "D:/Saves/MyGame".to_string(),
        include: vec!["**/*.sav".to_string()],
        exclude: vec!["logs/**".to_string()],
    };
    state.add_custom_game(&g).await.unwrap();

    // Duplicate normalized title is rejected.
    assert!(state.add_custom_game(&g).await.is_err());

    let all = state.list_custom_games().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].title, "My Cracked Game");
    assert_eq!(all[0].include, vec!["**/*.sav".to_string()]);
    assert_eq!(all[0].exclude, vec!["logs/**".to_string()]);

    state
        .remove_custom_game(&crate::naming::normalize_title("my cracked game"))
        .await
        .unwrap();
    assert!(state.list_custom_games().await.unwrap().is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savr-daemon custom_games_roundtrip`
Expected: FAIL — `CustomGame` / `add_custom_game` not found.

- [ ] **Step 3: Write minimal implementation**

3a. Add the table to the schema string (the block with `CREATE TABLE IF NOT EXISTS ...` near the top of `state.rs`, after `play_stats`):

```sql
CREATE TABLE IF NOT EXISTS custom_games (
    norm_title   TEXT PRIMARY KEY,
    title        TEXT NOT NULL,
    install_path TEXT,
    save_root    TEXT NOT NULL,
    include_glob TEXT NOT NULL,
    exclude_glob TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
```

3b. Add the struct near the other row structs in `state.rs`:

```rust
/// A hand-registered game not in any Steam library. Persisted because — unlike
/// Steam/auto-detected games — it can't be re-derived from a scan.
#[derive(Debug, Clone)]
pub struct CustomGame {
    pub title: String,
    pub install_path: Option<String>,
    pub save_root: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}
```

3c. Add the CRUD impl block on `LocalState` (glob lists are newline-joined; a blank line never occurs because empty patterns are filtered by the caller):

```rust
impl LocalState {
    pub async fn add_custom_game(&self, g: &CustomGame) -> anyhow::Result<()> {
        let norm = crate::naming::normalize_title(&g.title);
        anyhow::ensure!(!norm.is_empty(), "game title must contain letters or digits");
        sqlx::query(
            "INSERT INTO custom_games \
             (norm_title, title, install_path, save_root, include_glob, exclude_glob) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&norm)
        .bind(&g.title)
        .bind(&g.install_path)
        .bind(&g.save_root)
        .bind(g.include.join("\n"))
        .bind(g.exclude.join("\n"))
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow::anyhow!("could not add '{}': {e}", g.title))?;
        Ok(())
    }

    pub async fn list_custom_games(&self) -> anyhow::Result<Vec<CustomGame>> {
        let rows = sqlx::query(
            "SELECT title, install_path, save_root, include_glob, exclude_glob \
             FROM custom_games ORDER BY title",
        )
        .fetch_all(&self.pool)
        .await?;
        let split = |s: String| -> Vec<String> {
            s.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
        };
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(CustomGame {
                title: r.get::<String, _>("title"),
                install_path: r.get::<Option<String>, _>("install_path"),
                save_root: r.get::<String, _>("save_root"),
                include: split(r.get::<String, _>("include_glob")),
                exclude: split(r.get::<String, _>("exclude_glob")),
            });
        }
        Ok(out)
    }

    pub async fn remove_custom_game(&self, norm_title: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM custom_games WHERE norm_title = ?")
            .bind(norm_title)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
```

Ensure `use sqlx::Row;` is already imported in `state.rs` (it is — `list_roots` uses `r.get`). `CustomGame` must be re-exported if `engine.rs` refers to it as `crate::state::CustomGame` (it will) — it's `pub`, so no extra export needed.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p savr-daemon custom_games_roundtrip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/savr-daemon/src/state.rs
git commit -m "feat(daemon): persist hand-registered custom games"
```

---

## Task 4: Exclude-glob support in the snapshot pipeline

**Files:**
- Modify: `crates/savr-core/src/snapshot.rs`
- Modify: `crates/savr-daemon/src/paths.rs` (`ResolvedGame.excludes`)
- Modify: `crates/savr-daemon/src/backup.rs` (`BackupJob.excludes`, pass to build)
- Modify: `crates/savr-daemon/src/engine.rs` (`backup_job` copies excludes)
- Test: `crates/savr-core/src/snapshot.rs`

**Interfaces:**
- Produces: `Snapshot::build(game_id: GameId, patterns: &[String], excludes: &[String], anchor: &Path) -> Result<Snapshot>`. Excludes are glob patterns matched against each file's `rel_path` (separators normalized to `/`); a matching file is omitted.
- Consumes (updated): `ResolvedGame { patterns, registry_keys, anchor, excludes }`; `BackupJob { game_id, patterns, anchor, registry_keys, excludes }`.

- [ ] **Step 1: Write the failing test**

Add to `snapshot.rs` test module:

```rust
#[test]
fn build_honors_exclude_globs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("game.sav"), b"save").unwrap();
    std::fs::create_dir(dir.path().join("logs")).unwrap();
    std::fs::write(dir.path().join("logs").join("run.log"), b"noise").unwrap();

    let pattern = format!("{}/**/*", dir.path().display());
    let snap =
        Snapshot::build(GameId::now_v7(), &[pattern], &["logs/**".to_string()], dir.path())
            .unwrap();

    let paths: Vec<&str> = snap.files.iter().map(|f| f.rel_path.as_str()).collect();
    assert!(paths.contains(&"game.sav"));
    assert!(!paths.iter().any(|p| p.contains("run.log")), "logs excluded");
}
```

Also update the existing snapshot/backup tests and callers that call `Snapshot::build(...)` with 3 args to pass `&[]` for excludes (search: `grep -rn "Snapshot::build" crates`). Known call sites: `crates/savr-daemon/src/backup.rs` (run_backup + its tests), `crates/savr-core/src/snapshot.rs` tests.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savr-core build_honors_exclude_globs`
Expected: FAIL — arity mismatch / new test can't compile.

- [ ] **Step 3: Write minimal implementation**

3a. In `snapshot.rs`, change `build` and thread excludes into the walk. Replace the signature and pattern loop:

```rust
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
    let mut files = std::collections::BTreeMap::new();
    for pat in patterns {
        let matches = glob::glob(pat).map_err(|e| Error::Glob(e.to_string()))?;
        for path in matches.flatten() {
            collect(&path, anchor, &excludes, &mut files)?;
        }
    }
    // ... rest unchanged (build Snapshot from `files`) ...
}
```

3b. Update `collect` to take excludes and skip matching rel_paths:

```rust
fn collect(
    path: &Path,
    anchor: &Path,
    excludes: &[glob::Pattern],
    out: &mut BTreeMap<String, FileEntry>,
) -> Result<()> {
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            collect(&entry?.path(), anchor, excludes, out)?;
        }
        return Ok(());
    }
    let rel = rel_path(path, anchor);
    let rel_norm = rel.replace('\\', "/");
    if excludes.iter().any(|g| g.matches(&rel_norm)) {
        return Ok(());
    }
    // ... existing FileEntry construction using `rel` as the key ...
}
```

(Keep the existing body that computes size/mtime/hash and inserts into `out` keyed by `rel`.)

3c. `paths.rs`: add `pub excludes: Vec<String>` to `ResolvedGame`. In `resolve_game`, set `excludes: Vec::new()` in the returned struct (manifest games have no excludes).

3d. `backup.rs`: add `pub excludes: Vec<String>` to `BackupJob`; in `run_backup`, change the `Snapshot::build(job.game_id, &job.patterns, &job.anchor)` call to `Snapshot::build(job.game_id, &job.patterns, &job.excludes, &job.anchor)`.

3e. `engine.rs`: in `backup_job()`, add `excludes: self.resolved.excludes.clone(),`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p savr-core -p savr-daemon --lib && cargo test -p savr-daemon --test ipc_dispatch`
Expected: PASS, including the new exclude test and all previously-passing tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/savr-core/src/snapshot.rs crates/savr-daemon/src/paths.rs crates/savr-daemon/src/backup.rs crates/savr-daemon/src/engine.rs
git commit -m "feat: exclude-glob support in the snapshot pipeline"
```

---

## Task 5: Name-based cross-device identity

**Files:**
- Modify: `crates/savr-daemon/src/engine.rs` (`game_id_for` signature + key derivation, and its Steam call site)

**Interfaces:**
- Produces: `async fn game_id_for(&self, appid: Option<u32>, title: &str, authed: bool) -> GameId`. When `appid` is `Some(a)`, key `gameid:steam:{a}` and `ensure_game(title, Some(a))` (unchanged behavior). When `None`, key `gameid:name:{normalize_title(title)}` and `ensure_game(title, None)`.

- [ ] **Step 1: Change the signature and key derivation**

In `engine.rs`, change the first line of `game_id_for` and the key:

```rust
async fn game_id_for(&self, appid: Option<u32>, title: &str, authed: bool) -> GameId {
    let key = match appid {
        Some(a) => format!("gameid:steam:{a}"),
        None => format!("gameid:name:{}", crate::naming::normalize_title(title)),
    };
    // ... unchanged read/cached logic ...
    if authed {
        match self.client.ensure_game(title, appid).await {
            // ... unchanged ...
        }
    }
    // ... unchanged ...
}
```

Note: `ensure_game` already takes `Option<u32>` (the Steam call passes `Some(appid)`), so `ensure_game(title, appid)` type-checks directly.

- [ ] **Step 2: Update the existing Steam call site**

In `refresh_games`, change `self.game_id_for(sg.appid, &title, authed)` to `self.game_id_for(Some(sg.appid), &title, authed)`.

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo test -p savr-daemon --lib`
Expected: PASS (no behavior change yet; existing tests still green).

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
git add crates/savr-daemon/src/engine.rs
git commit -m "feat(daemon): name-based game identity for non-Steam games"
```

---

## Task 6: Wire both sources into `refresh_games`

**Files:**
- Modify: `crates/savr-daemon/src/paths.rs` (`resolve_game` `base_override` + `resolve_custom`)
- Modify: `crates/savr-daemon/src/engine.rs` (`refresh_games` two new source loops + dedup)
- Test: `crates/savr-daemon/tests/` — a new integration test `custom_catalog.rs`

**Interfaces:**
- Produces:
  - `resolve_game(game, overrides, ctx, base_override: Option<&Path>) -> ResolvedGame` — `base` becomes `base_override.map(Path::to_path_buf).or_else(|| ctx.install_dir(game.steam_appid))`.
  - `pub fn resolve_custom(save_root: &str, include: &[String], exclude: &[String]) -> ResolvedGame` in `paths.rs`.
- Consumes: `ManifestMatcher`, `scan_folder_root`, `LocalState::list_custom_games`, `game_id_for(None, ..)`.

- [ ] **Step 1: Add `resolve_custom` and `base_override`, with a unit test**

1a. In `paths.rs`, change `resolve_game`'s signature to add `base_override: Option<&Path>` and its first line:

```rust
pub fn resolve_game(
    game: &Game,
    overrides: &[PathOverride],
    ctx: &ResolveContext,
    base_override: Option<&Path>,
) -> ResolvedGame {
    let base = base_override
        .map(Path::to_path_buf)
        .or_else(|| ctx.install_dir(game.steam_appid));
    // ... rest unchanged, but set `excludes: Vec::new()` in the returned struct ...
```

Update the existing call in `refresh_games` to pass `None` for now (the Steam loop): `resolve_game(&game, &overrides, &ctx, None)`. Update any `resolve_game` calls in `paths.rs` tests to pass `None`.

1b. Add `resolve_custom` and its test to `paths.rs`:

```rust
/// Resolve a hand-registered game's save location: join each include glob onto
/// the save root, keep the excludes for the snapshot walk, and anchor on the
/// save root so rel_paths are stable. Separators are normalized to `/` so the
/// `glob` crate (which treats `\` as an escape) works on Windows paths.
pub fn resolve_custom(save_root: &str, include: &[String], exclude: &[String]) -> ResolvedGame {
    let root = save_root.replace('\\', "/");
    let root = root.trim_end_matches('/');
    let includes = if include.is_empty() {
        vec!["**/*".to_string()]
    } else {
        include.to_vec()
    };
    let patterns = includes
        .iter()
        .map(|g| format!("{root}/{}", g.trim_start_matches('/')))
        .collect();
    ResolvedGame {
        patterns,
        registry_keys: Vec::new(),
        anchor: PathBuf::from(root),
        excludes: exclude.to_vec(),
    }
}
```

Test:

```rust
#[test]
fn resolve_custom_joins_globs_onto_save_root() {
    let r = resolve_custom("D:\\Saves\\Game", &["**/*.sav".into()], &["logs/**".into()]);
    assert_eq!(r.patterns, vec!["D:/Saves/Game/**/*.sav".to_string()]);
    assert_eq!(r.anchor, std::path::PathBuf::from("D:/Saves/Game"));
    assert_eq!(r.excludes, vec!["logs/**".to_string()]);

    let d = resolve_custom("/home/u/saves", &[], &[]);
    assert_eq!(d.patterns, vec!["/home/u/saves/**/*".to_string()]);
}
```

- [ ] **Step 2: Run the resolve tests**

Run: `cargo test -p savr-daemon resolve_custom`
Expected: PASS.

- [ ] **Step 3: Extend `refresh_games` with the two new sources**

In `engine.rs`, inside `refresh_games`, after the `for lib in &steam_libs { ... }` loop and before `let n = games.len();`, add:

```rust
// Track normalized titles already in the catalog so a later source never
// duplicates a game an earlier (higher-precedence) source already added.
let mut seen: std::collections::HashSet<String> = games
    .values()
    .map(|e| crate::naming::normalize_title(&e.game.title))
    .collect();

// Capability A: auto-detect manifest-known games under generic "game folder"
// (Drive) roots by matching each install-folder name to the manifest.
let matcher = crate::scan::ManifestMatcher::build(&manifest);
for root in self.state.list_roots().await.unwrap_or_default() {
    if root.kind != RootKind::Drive {
        continue;
    }
    for (folder_name, install_dir) in crate::scan::scan_folder_root(std::path::Path::new(&root.path)) {
        let Some(title) = matcher.match_folder(&folder_name) else {
            continue;
        };
        let norm = crate::naming::normalize_title(&title);
        if !seen.insert(norm) {
            continue;
        }
        let Some(entry) = manifest.get(&title) else { continue };
        if self.config.games.get(title.as_str()).map(|g| g.ignore).unwrap_or(false) {
            continue;
        }
        let game_id = self.game_id_for(None, &title, authed).await;
        let game = Game {
            id: game_id,
            title: title.clone(),
            source: GameSource::Manifest,
            steam_appid: None,
            save_targets: entry.save_targets(),
            running: false,
            last_played: None,
            last_session_secs: None,
            total_secs: 0,
        };
        let resolved = resolve_game(&game, &overrides, &ctx, Some(&install_dir));
        index.index_install_dir(&install_dir, game_id);
        games.insert(game_id, GameEntry { game, resolved });
    }
}

// Capability B: hand-registered custom games (persisted).
for cg in self.state.list_custom_games().await.unwrap_or_default() {
    let norm = crate::naming::normalize_title(&cg.title);
    if !seen.insert(norm) {
        continue;
    }
    let game_id = self.game_id_for(None, &cg.title, authed).await;
    let game = Game {
        id: game_id,
        title: cg.title.clone(),
        source: GameSource::Custom,
        steam_appid: None,
        save_targets: Vec::new(),
        running: false,
        last_played: None,
        last_session_secs: None,
        total_secs: 0,
    };
    let resolved = resolve_custom(&cg.save_root, &cg.include, &cg.exclude);
    if let Some(p) = &cg.install_path {
        let path = std::path::Path::new(p);
        if path.is_dir() {
            index.index_install_dir(path, game_id);
        } else {
            index.insert_exe(path, game_id);
        }
    }
    games.insert(game_id, GameEntry { game, resolved });
}
```

Confirm imports at the top of `engine.rs` include `GameSource` and `resolve_custom` (add `use crate::paths::resolve_custom;` or call as `crate::paths::resolve_custom`). `manifest.get(&title)` — confirm `Manifest` exposes `get`; if it only exposes `iter()`, build a `HashMap<&str, &ManifestEntry>` from `manifest.iter()` once and look up there instead.

- [ ] **Step 4: Write the integration test**

Create `crates/savr-daemon/tests/custom_catalog.rs`:

```rust
//! refresh_games merges Steam + auto-detected + manual games into one catalog,
//! deduped by normalized title.

use std::sync::Arc;

use savr_daemon::config::DaemonConfig;
use savr_daemon::engine::Engine;
use savr_daemon::secrets::{FileStore, SecretStore};
use savr_daemon::state::{CustomGame, LocalState};
use tokio::sync::broadcast;

async fn engine_with(manifest_yaml: &str, drive_root: &std::path::Path) -> Arc<Engine> {
    let state = LocalState::open_memory().await.unwrap();
    state
        .add_root(savr_core::ipc::RootKind::Drive, &drive_root.to_string_lossy())
        .await
        .unwrap();
    state
        .add_custom_game(&CustomGame {
            title: "My Cracked Game".into(),
            install_path: None,
            save_root: drive_root.join("saves").to_string_lossy().into_owned(),
            include: vec!["**/*".into()],
            exclude: vec![],
        })
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let secret_store: Arc<dyn SecretStore> =
        Arc::new(FileStore::new(dir.path().join("creds.json")));
    std::mem::forget(dir);
    let (events, _rx) = broadcast::channel(16);
    let engine = Engine::new(DaemonConfig::default(), state, secret_store, events)
        .await
        .unwrap();
    let manifest = savr_core::manifest::parse(manifest_yaml).unwrap();
    engine.set_manifest(manifest).await;
    engine
}

#[tokio::test]
async fn merges_autodetected_and_custom_games() {
    let drive = tempfile::tempdir().unwrap();
    std::fs::create_dir(drive.path().join("Hollow Knight")).unwrap();
    let manifest = "\
Hollow Knight:
  files:
    <base>/saves: {}
  installDir:
    Hollow Knight: {}
";
    let engine = engine_with(manifest, drive.path()).await;
    engine.refresh_games(false).await.unwrap();

    let games = engine.list_games_for_test().await; // see note below
    let titles: std::collections::HashSet<String> =
        games.iter().map(|g| g.title.clone()).collect();
    assert!(titles.contains("Hollow Knight"), "auto-detected");
    assert!(titles.contains("My Cracked Game"), "manual");
}
```

If `Engine` has no public accessor for the catalog in tests, add a minimal one guarded for reuse: `pub async fn list_games_for_test(&self) -> Vec<savr_core::types::Game>` returning `self.games.read().await.values().map(|e| e.game.clone()).collect()`. If a public `ListGames` handler path is easier, call `engine.handle_request(GuiRequest::ListGames)` and match `DaemonMsg::Games`. Prefer whichever the codebase already exposes; do not add broad public surface beyond this test helper.

- [ ] **Step 5: Run the integration test**

Run: `cargo test -p savr-daemon --test custom_catalog`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/savr-daemon/src/paths.rs crates/savr-daemon/src/engine.rs crates/savr-daemon/tests/custom_catalog.rs
git commit -m "feat(daemon): auto-detect and custom games in the catalog"
```

---

## Task 7: IPC requests, daemon handlers, app commands

**Files:**
- Modify: `crates/savr-core/src/ipc.rs` (`CustomGameSpec`, two `GuiRequest` variants, encode tests)
- Modify: `crates/savr-daemon/src/engine.rs` (handle the two variants)
- Modify: `crates/savr-app/src-tauri/src/commands.rs` + `lib.rs` (two commands)
- Modify: `crates/savr-app/ui/src/lib/api.ts`, `types.ts`

**Interfaces:**
- Produces:
  - `pub struct CustomGameSpec { pub title: String, pub install_path: Option<String>, pub save_root: String, pub include: Vec<String>, pub exclude: Vec<String> }` in `ipc.rs`.
  - `GuiRequest::AddCustomGame { spec: CustomGameSpec }`, `GuiRequest::RemoveCustomGame { title: String }`.
  - Tauri commands `add_custom_game(spec: CustomGameSpec)`, `remove_custom_game(title: String)`.

- [ ] **Step 1: Add the IPC types + encode test (failing)**

In `ipc.rs`, add the struct (near `RootSpec`):

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CustomGameSpec {
    pub title: String,
    pub install_path: Option<String>,
    pub save_root: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}
```

Add variants to `GuiRequest` (struct variants — safe under internal tagging):

```rust
    /// Register a game not found in any Steam library.
    AddCustomGame {
        spec: CustomGameSpec,
    },
    /// Remove a hand-registered game by its title.
    RemoveCustomGame {
        title: String,
    },
```

Add encode tests in the ipc test module:

```rust
#[test]
fn add_custom_game_encodes() {
    let req = GuiRequest::AddCustomGame {
        spec: CustomGameSpec {
            title: "X".into(),
            install_path: None,
            save_root: "/s".into(),
            include: vec!["**/*".into()],
            exclude: vec![],
        },
    };
    let frame = encode_frame(&req).expect("must encode");
    let back: GuiRequest = serde_json::from_slice(&frame[4..]).unwrap();
    assert!(matches!(back, GuiRequest::AddCustomGame { .. }));
}

#[test]
fn remove_custom_game_encodes() {
    let frame = encode_frame(&GuiRequest::RemoveCustomGame { title: "X".into() }).unwrap();
    let back: GuiRequest = serde_json::from_slice(&frame[4..]).unwrap();
    assert!(matches!(back, GuiRequest::RemoveCustomGame { .. }));
}
```

Run: `cargo test -p savr-core --lib --features ipc add_custom_game_encodes remove_custom_game_encodes`
Expected: FAIL (variants not yet present) → then PASS after adding them. (They're added in this same step, so this is really "run and confirm PASS".)

- [ ] **Step 2: Handle the variants in the engine**

In `engine.rs` `handle_request`, add arms (before the `Shutdown` arm):

```rust
GuiRequest::AddCustomGame { spec } => {
    let cg = crate::state::CustomGame {
        title: spec.title,
        install_path: spec.install_path,
        save_root: spec.save_root,
        include: spec.include,
        exclude: spec.exclude,
    };
    match self.state.add_custom_game(&cg).await {
        Ok(()) => {
            if let Err(e) = self.refresh_games(true).await {
                tracing::warn!("refresh after add_custom_game failed: {e}");
            }
            DaemonMsg::Ok
        }
        Err(e) => err(e),
    }
}
GuiRequest::RemoveCustomGame { title } => {
    let norm = crate::naming::normalize_title(&title);
    match self.state.remove_custom_game(&norm).await {
        Ok(()) => {
            let _ = self.refresh_games(true).await;
            DaemonMsg::Ok
        }
        Err(e) => err(e),
    }
}
```

Run: `cargo test -p savr-daemon --lib`
Expected: PASS (exhaustive match now covers new variants).

- [ ] **Step 3: Add the Tauri commands**

In `crates/savr-app/src-tauri/src/commands.rs` (import `CustomGameSpec` in the existing `use savr_core::ipc::{...}` line):

```rust
#[tauri::command]
pub async fn add_custom_game(spec: CustomGameSpec) -> Result<(), CmdError> {
    expect_ok(request(GuiRequest::AddCustomGame { spec }).await?)
}

#[tauri::command]
pub async fn remove_custom_game(title: String) -> Result<(), CmdError> {
    expect_ok(request(GuiRequest::RemoveCustomGame { title }).await?)
}
```

Register both in `lib.rs` `tauri::generate_handler![...]` (after `pair_device,`):

```rust
            commands::add_custom_game,
            commands::remove_custom_game,
```

Run: `cargo check -p savr-app`
Expected: compiles.

- [ ] **Step 4: Front-end api + types**

In `crates/savr-app/ui/src/lib/types.ts`, add:

```ts
export interface CustomGameSpec {
  title: string;
  install_path: string | null;
  save_root: string;
  include: string[];
  exclude: string[];
}
```

Confirm `Game.source` type includes `"Custom"` (it maps the Rust `GameSource`); if `source` is typed as a string union, add `"Custom"`.

In `crates/savr-app/ui/src/lib/api.ts`, add:

```ts
export const addCustomGame = (spec: CustomGameSpec) =>
  invoke<void>("add_custom_game", { spec });

export const removeCustomGame = (title: string) =>
  invoke<void>("remove_custom_game", { title });
```

Run: `cd crates/savr-app/ui && pnpm check`
Expected: 0 errors.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/savr-core/src/ipc.rs crates/savr-daemon/src/engine.rs crates/savr-app/src-tauri/src/commands.rs crates/savr-app/src-tauri/src/lib.rs crates/savr-app/ui/src/lib/api.ts crates/savr-app/ui/src/lib/types.ts
git commit -m "feat: add/remove custom games over IPC"
```

---

## Task 8: UI — add a game folder & add a manual game

**Files:**
- Create: `crates/savr-app/ui/src/views/AddGameDialog.svelte`
- Modify: `crates/savr-app/ui/src/views/Games.svelte`

**Interfaces:**
- Consumes: `addRoot`, `addCustomGame`, `removeCustomGame` from `api.ts`; `open` from `@tauri-apps/plugin-dialog` for pickers.

- [ ] **Step 1: Add a "game folder" root action to Games.svelte**

Add a button that picks a folder and registers it as a `Drive` root, then reloads:

```svelte
<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { addRoot, addCustomGame, removeCustomGame } from "../lib/api";
  // ...existing imports/state, incl. the games list + a reload() function...

  async function addGameFolder() {
    const dir = await open({ directory: true, title: "Pick a folder that contains your games" });
    if (typeof dir === "string") {
      await addRoot({ kind: "drive", path: dir });
      await reload();
    }
  }
</script>

<button class="secondary" on:click={addGameFolder}>Add game folder</button>
```

(Use the existing `reload`/`load` function name in Games.svelte; match its styling classes.)

- [ ] **Step 2: Build the manual-add dialog**

Create `AddGameDialog.svelte` — a form with title, install path (file/folder picker, optional), save folder (folder picker, required), and include/exclude textareas (one glob per line). On submit it calls `addCustomGame` and emits a `saved` event.

```svelte
<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { addCustomGame } from "../lib/api";

  const dispatch = createEventDispatcher();
  let title = "";
  let installPath = "";
  let saveRoot = "";
  let includeText = "**/*";
  let excludeText = "";
  let error = "";
  let busy = false;

  async function pickInstall() {
    const p = await open({ title: "Game .exe or install folder" });
    if (typeof p === "string") installPath = p;
  }
  async function pickSave() {
    const p = await open({ directory: true, title: "Save folder" });
    if (typeof p === "string") saveRoot = p;
  }
  const lines = (s: string) => s.split("\n").map((l) => l.trim()).filter(Boolean);

  async function submit() {
    error = "";
    if (!title.trim()) { error = "Give the game a name."; return; }
    if (!saveRoot.trim()) { error = "Pick the save folder."; return; }
    busy = true;
    try {
      await addCustomGame({
        title: title.trim(),
        install_path: installPath.trim() || null,
        save_root: saveRoot.trim(),
        include: lines(includeText),
        exclude: lines(excludeText),
      });
      dispatch("saved");
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<!-- Minimal modal markup; match the app's existing modal/panel styling. -->
<div class="dialog">
  <h3>Add a game</h3>
  <label>Name<input bind:value={title} placeholder="e.g. Elden Ring" /></label>
  <label>Install .exe / folder (optional, for detection)
    <div class="row"><input bind:value={installPath} readonly /><button on:click={pickInstall}>Browse…</button></div>
  </label>
  <label>Save folder
    <div class="row"><input bind:value={saveRoot} readonly /><button on:click={pickSave}>Browse…</button></div>
  </label>
  <label>Include globs (one per line)<textarea bind:value={includeText} rows="2"></textarea></label>
  <label>Exclude globs (one per line)<textarea bind:value={excludeText} rows="2"></textarea></label>
  {#if error}<p class="error">{error}</p>{/if}
  <div class="actions">
    <button on:click={() => dispatch("close")}>Cancel</button>
    <button class="primary" disabled={busy} on:click={submit}>Add game</button>
  </div>
</div>
```

- [ ] **Step 3: Wire the dialog + remove button into Games.svelte**

Add an "Add game" button that opens `AddGameDialog`; on its `saved` event, close and `reload()`. For each game where `game.source === "Custom"`, render a small "Remove" action calling `removeCustomGame(game.title)` then `reload()`.

```svelte
<script lang="ts">
  import AddGameDialog from "./AddGameDialog.svelte";
  let showAdd = false;
  async function removeGame(title: string) {
    await removeCustomGame(title);
    await reload();
  }
</script>

<button class="primary" on:click={() => (showAdd = true)}>Add game</button>
{#if showAdd}
  <AddGameDialog on:saved={() => { showAdd = false; reload(); }} on:close={() => (showAdd = false)} />
{/if}

<!-- in the per-game row, alongside existing actions: -->
{#if game.source === "Custom"}
  <button class="danger" on:click={() => removeGame(game.title)}>Remove</button>
{/if}
```

- [ ] **Step 4: Verify the front-end compiles**

Run: `cd crates/savr-app/ui && pnpm check`
Expected: 0 errors, 0 warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/savr-app/ui/src/views/AddGameDialog.svelte crates/savr-app/ui/src/views/Games.svelte
git commit -m "feat(app): UI to add game folders and custom games"
```

---

## Task 9: Full verification pass

**Files:** none (verification only).

- [ ] **Step 1: Workspace tests, clippy, fmt, svelte**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo test -p savr-core --lib --features ipc
cd crates/savr-app/ui && pnpm check
```

Expected: fmt clean, clippy no warnings, all tests pass (including `naming`, `scan`, `custom_games_roundtrip`, `build_honors_exclude_globs`, `resolve_custom_joins_globs_onto_save_root`, `merges_autodetected_and_custom_games`, the two ipc encode tests), svelte 0 errors.

- [ ] **Step 2: Manual smoke checklist (documented, run on Windows before release)**

- Add a game folder pointing at a directory containing a manifest-known game's install folder → it appears in the Games list with its canonical name.
- Launch that game → "Playing" badge appears (detection via indexed exe).
- Add a manual game with a save folder + `**/*` include and a `logs/**` exclude → appears with source Custom; "Back up now" creates a version whose files exclude the logs folder.
- Remove the manual game → it disappears and stays gone after a restart.

- [ ] **Step 3: (No commit unless fixes were needed.)**

---

## Self-review notes

- **Spec coverage:** auto-detect (Tasks 2, 6) ✓; manual add with folder+globs (Tasks 3, 4, 6) ✓; persisted custom store (Task 3) ✓; catalog merge with precedence (Task 6 `seen` set) ✓; name-based cross-device identity (Task 5) ✓; IPC/commands/UI (Tasks 7, 8) ✓; exclude globs (Task 4) ✓; ambiguous-match-skipped (Task 2 test) ✓; error handling — unreadable roots skipped (`scan_folder_root` returns empty; `list_roots`/`list_custom_games` use `unwrap_or_default`) ✓.
- **Precedence** is enforced by insertion order (Steam → auto-detect → custom) plus the `seen` normalized-title set, matching the spec's "Steam-manifest > auto-detect > manual".
- **`Manifest::get`** is assumed; Task 6 Step 3 notes the fallback (build a lookup map from `iter()`) if it isn't public.
- **Out of scope (Phases 2/3):** real Learn mode, launcher-specific auto-config, similarity-ratio fuzzy matching, recursive scanning — none implemented here.
</content>
</invoke>
