//! Turn a game's save *templates* into concrete filesystem globs (PRD-02 §1.3,
//! PRD-03 §1 step 1). Wraps `savr_core::manifest::resolve`, layers in the
//! account's per-game overrides, and — for Steam+Proton on Linux — resolves the
//! Windows placeholders *inside* the game's Proton prefix, mirroring how
//! Ludusavi handles Proton (PRD-02 §2, §4).

use std::path::{Component, Path, PathBuf};

use savr_core::manifest::{self, Roots};
use savr_core::types::Os;
use savr_core::{Game, PathOverride};

use crate::detection::steam::{steam_account_ids, SteamLibrary};

/// The resolved, ready-to-glob save locations for one game.
#[derive(Debug, Clone)]
pub struct ResolvedGame {
    /// Filesystem glob patterns (placeholders expanded).
    pub patterns: Vec<String>,
    /// Windows registry keys to capture (HKCU). Non-empty only for games whose
    /// manifest declares `registry:` targets.
    pub registry_keys: Vec<String>,
    /// Stable anchor the snapshot's `rel_path`s are computed against, so the
    /// same save set hashes identically across machines.
    pub anchor: PathBuf,
    /// Glob patterns matched against each file's anchor-relative path;
    /// matching files are omitted from the snapshot. Empty for manifest/Steam
    /// games — populated for manually-added games (PRD-02 §1.5).
    pub excludes: Vec<String>,
}

/// Everything path resolution needs from the environment.
pub struct ResolveContext<'a> {
    pub roots: &'a Roots,
    pub steam_libs: &'a [SteamLibrary],
}

impl<'a> ResolveContext<'a> {
    /// The install dir of a Steam game (`<base>`), if we can find it.
    fn install_dir(&self, appid: Option<u32>) -> Option<PathBuf> {
        let appid = appid?;
        for lib in self.steam_libs {
            if let Some(g) = lib.games.iter().find(|g| g.appid == appid) {
                return Some(lib.install_path(g));
            }
        }
        None
    }

    /// Proton prefix roots for a game, one per library that has a compatdata
    /// prefix for it. Windows placeholders resolve inside `drive_c`.
    fn proton_roots(&self, appid: Option<u32>) -> Vec<Roots> {
        let Some(appid) = appid else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for lib in self.steam_libs {
            let drive_c = lib.proton_drive_c(appid);
            if drive_c.exists() {
                out.push(proton_prefix_roots(self.roots, &drive_c));
            }
        }
        out
    }
}

/// Build a `Roots` that points the Windows placeholders at a Proton prefix's
/// `drive_c/users/steamuser/...` (PRD-02 §2).
fn proton_prefix_roots(base: &Roots, drive_c: &Path) -> Roots {
    let user = drive_c.join("users").join("steamuser");
    Roots {
        home: user.clone(),
        win_app_data: Some(user.join("AppData").join("Roaming")),
        win_local_app_data: Some(user.join("AppData").join("Local")),
        win_documents: Some(user.join("Documents")),
        // xdg_* stay as the host's — Proton games occasionally use them too.
        xdg_data: base.xdg_data.clone(),
        xdg_config: base.xdg_config.clone(),
        os_user_name: Some("steamuser".to_string()),
        // Force Windows so windows-only templates are considered.
        os: Os::Windows,
    }
}

/// Resolve a game's save targets into concrete globs, applying any account
/// override for it. `base_override`, when set, supplies `<base>` directly
/// (e.g. a non-Steam game's scanned install dir) instead of looking it up via
/// a Steam appid.
pub fn resolve_game(
    game: &Game,
    overrides: &[PathOverride],
    ctx: &ResolveContext,
    base_override: Option<&Path>,
) -> ResolvedGame {
    let base = base_override
        .map(Path::to_path_buf)
        .or_else(|| ctx.install_dir(game.steam_appid));
    let proton = ctx.proton_roots(game.steam_appid);

    let mut patterns: Vec<String> = Vec::new();
    let mut registry_keys: Vec<String> = Vec::new();

    // Collect the templates to resolve (an override redirects the whole game,
    // PRD-02 §1.4; otherwise use the manifest targets, siphoning off registry
    // keys which are captured separately).
    let mut templates: Vec<&str> = Vec::new();
    if let Some(ov) = overrides.iter().find(|o| o.game_id == game.id) {
        templates.extend(ov.globs.iter().map(String::as_str));
    } else {
        for target in &game.save_targets {
            if target.registry {
                push_unique_str(&mut registry_keys, &target.glob);
            } else {
                templates.push(&target.glob);
            }
        }
    }

    for tpl in templates {
        // Fill Steam's <root> / <storeUserId> (cloud userdata saves) that core
        // resolve can't — one concrete template per (library root, signed-in
        // account). A template with neither placeholder passes through unchanged.
        for concrete in expand_steam_placeholders(tpl, ctx.steam_libs) {
            if let Some(r) = manifest::resolve(&concrete, ctx.roots, base.as_deref()) {
                push_unique(&mut patterns, r);
            }
        }
        for pr in &proton {
            if let Some(r) = manifest::resolve(tpl, pr, base.as_deref()) {
                push_unique(&mut patterns, r);
            }
        }
    }

    let anchor = compute_anchor(&patterns, ctx.roots);
    ResolvedGame {
        patterns,
        registry_keys,
        anchor,
        excludes: Vec::new(),
    }
}

/// Resolve a hand-registered game's save location: join each include glob onto
/// the save root, keep the excludes for the snapshot walk, and anchor on the
/// save root so rel_paths are stable. Separators are normalized to `/` so the
/// `glob` crate (which treats `\` as an escape) works on Windows paths.
pub fn resolve_custom(save_root: &str, include: &[String], exclude: &[String]) -> ResolvedGame {
    let root = save_root.replace('\\', "/");
    let root = root.trim_end_matches('/');
    let includes: Vec<String> = if include.is_empty() {
        vec!["**/*".to_string()]
    } else {
        include.iter().map(|g| g.replace('\\', "/")).collect()
    };
    let patterns = includes
        .iter()
        .map(|g| format!("{root}/{}", g.trim_start_matches('/')))
        .collect();
    let excludes = exclude.iter().map(|g| g.replace('\\', "/")).collect();
    ResolvedGame {
        patterns,
        registry_keys: Vec::new(),
        anchor: PathBuf::from(root),
        excludes,
    }
}

/// Expand Steam store placeholders (`<root>`, `<storeUserId>`) that
/// `manifest::resolve` leaves untouched, yielding one concrete template per
/// (Steam library root, signed-in account). A template with neither placeholder
/// returns unchanged. When a placeholder can't be filled — no Steam libraries,
/// or `<storeUserId>` but no accounts under any `userdata/` — it yields nothing,
/// so the target is skipped (same contract as `resolve` returning `None`).
fn expand_steam_placeholders(tpl: &str, steam_libs: &[SteamLibrary]) -> Vec<String> {
    let needs_root = tpl.contains("<root>");
    let needs_uid = tpl.contains("<storeUserId>");
    if !needs_root && !needs_uid {
        return vec![tpl.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    for lib in steam_libs {
        let root = lib.path.to_string_lossy();
        if needs_uid {
            // Accounts live under the *main* Steam root's userdata; secondary
            // libraries simply return none, so they contribute nothing here.
            for uid in steam_account_ids(&lib.userdata_dir()) {
                let mut s = tpl.to_string();
                if needs_root {
                    s = s.replace("<root>", &root);
                }
                push_unique(&mut out, s.replace("<storeUserId>", &uid));
            }
        } else {
            push_unique(&mut out, tpl.replace("<root>", &root));
        }
    }
    out
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

fn push_unique_str(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// The common ancestor directory of every pattern's fixed prefix. Falls back to
/// the user's home when there are no patterns.
fn compute_anchor(patterns: &[String], roots: &Roots) -> PathBuf {
    if patterns.is_empty() {
        return roots.home.clone();
    }
    let candidates: Vec<PathBuf> = patterns.iter().map(|p| anchor_candidate(p)).collect();
    common_ancestor(&candidates).unwrap_or_else(|| roots.home.clone())
}

fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '[', ']', '{', '}'])
}

/// The directory to anchor rel-paths at for one pattern: the fixed (glob-free)
/// leading directory, or the parent of a fully-concrete file path.
fn anchor_candidate(pattern: &str) -> PathBuf {
    let mut acc = PathBuf::new();
    let mut hit_glob = false;
    for comp in Path::new(pattern).components() {
        let s = comp.as_os_str().to_string_lossy();
        if is_glob(&s) {
            hit_glob = true;
            break;
        }
        acc.push(comp.as_os_str());
    }
    if hit_glob {
        acc
    } else {
        acc.parent().map(Path::to_path_buf).unwrap_or(acc)
    }
}

fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let first = iter.next()?;
    let mut prefix: Vec<Component> = first.components().collect();
    for p in iter {
        let comps: Vec<Component> = p.components().collect();
        let shared = prefix
            .iter()
            .zip(comps.iter())
            .take_while(|(a, b)| a == b)
            .count();
        prefix.truncate(shared);
    }
    let mut out = PathBuf::new();
    for c in prefix {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// Capture Windows HKCU registry keys into a `registry.json` blob (PRD-02 §4).
///
/// ponytail: real HKCU export/import (via the `windows` / `winreg` crate) is a
/// documented stub — it can't be exercised from macOS/Linux where this is
/// built and tested. The *shape* is fully wired: keys flow from `resolve_game`
/// into `ArchiveMeta` and the archive's `registry.json`, and restore threads
/// the bytes back out (`archive::Unpacked.registry`). Only the actual registry
/// read/write is TODO for the Windows milestone.
#[cfg(windows)]
pub fn capture_registry(keys: &[String]) -> anyhow::Result<Option<Vec<u8>>> {
    if keys.is_empty() {
        return Ok(None);
    }
    // TODO(windows): read each HKCU key recursively and serialize to JSON.
    tracing::warn!(
        "registry capture not yet implemented; {} keys skipped",
        keys.len()
    );
    Ok(None)
}

#[cfg(not(windows))]
pub fn capture_registry(_keys: &[String]) -> anyhow::Result<Option<Vec<u8>>> {
    // Registry only exists on Windows; nothing to capture elsewhere.
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_candidate_fixed_prefix() {
        assert_eq!(
            anchor_candidate("/home/me/.local/share/Game/saves/**/*"),
            PathBuf::from("/home/me/.local/share/Game/saves")
        );
        // Concrete file → its parent dir.
        assert_eq!(
            anchor_candidate("/home/me/.config/game/settings.json"),
            PathBuf::from("/home/me/.config/game")
        );
    }

    #[test]
    fn common_ancestor_of_two_targets() {
        let patterns = vec![
            "/home/me/.local/share/Game/saves/**/*".to_string(),
            "/home/me/.local/share/Game/settings.json".to_string(),
        ];
        let roots = test_roots();
        let anchor = compute_anchor(&patterns, &roots);
        assert_eq!(anchor, PathBuf::from("/home/me/.local/share/Game"));
    }

    #[test]
    fn empty_patterns_anchor_home() {
        let roots = test_roots();
        assert_eq!(compute_anchor(&[], &roots), roots.home);
    }

    #[test]
    fn expands_steam_root_and_user_id() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // One signed-in account plus noise that must be ignored.
        std::fs::create_dir_all(root.join("userdata/76561198000000000/588650/remote")).unwrap();
        std::fs::create_dir_all(root.join("userdata/0")).unwrap();
        let libs = vec![SteamLibrary {
            path: root.to_path_buf(),
            games: vec![],
        }];

        // <root> + <storeUserId> -> one concrete template for the real account.
        let got = expand_steam_placeholders(
            "<root>/userdata/<storeUserId>/588650/remote/user_*.dat",
            &libs,
        );
        assert_eq!(
            got,
            vec![format!(
                "{}/userdata/76561198000000000/588650/remote/user_*.dat",
                root.display()
            )]
        );

        // <root> only -> per library, no account needed.
        assert_eq!(
            expand_steam_placeholders("<root>/steamapps/common/Foo/save", &libs),
            vec![format!("{}/steamapps/common/Foo/save", root.display())]
        );

        // No store placeholder -> unchanged, single entry.
        assert_eq!(
            expand_steam_placeholders("<base>/save/x", &libs),
            vec!["<base>/save/x".to_string()]
        );

        // <storeUserId> but no accounts anywhere -> yields nothing (skipped).
        let empty = tempfile::tempdir().unwrap();
        let no_accounts = vec![SteamLibrary {
            path: empty.path().to_path_buf(),
            games: vec![],
        }];
        assert!(
            expand_steam_placeholders("<root>/userdata/<storeUserId>/1/remote", &no_accounts)
                .is_empty()
        );
    }

    #[test]
    fn resolve_custom_joins_globs_onto_save_root() {
        let r = resolve_custom("D:\\Saves\\Game", &["**/*.sav".into()], &["logs/**".into()]);
        assert_eq!(r.patterns, vec!["D:/Saves/Game/**/*.sav".to_string()]);
        assert_eq!(r.anchor, std::path::PathBuf::from("D:/Saves/Game"));
        assert_eq!(r.excludes, vec!["logs/**".to_string()]);

        let d = resolve_custom("/home/u/saves", &[], &[]);
        assert_eq!(d.patterns, vec!["/home/u/saves/**/*".to_string()]);

        // Windows-style backslash globs (as a user might type them) must be
        // normalized to forward slashes too — the `glob` crate treats `\` as
        // an escape, and excludes are matched against `/`-normalized rel_paths.
        let bs = resolve_custom(
            "D:\\Saves\\Game",
            &["saves\\**".into()],
            &["logs\\**".into()],
        );
        assert_eq!(bs.patterns, vec!["D:/Saves/Game/saves/**".to_string()]);
        assert_eq!(bs.excludes, vec!["logs/**".to_string()]);
    }

    fn test_roots() -> Roots {
        Roots {
            home: PathBuf::from("/home/me"),
            win_app_data: None,
            win_local_app_data: None,
            win_documents: None,
            xdg_data: Some(PathBuf::from("/home/me/.local/share")),
            xdg_config: Some(PathBuf::from("/home/me/.config")),
            os_user_name: Some("me".into()),
            os: Os::Linux,
        }
    }
}
