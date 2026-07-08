//! Ludusavi-style detection: match a game's install-folder name against the
//! Ludusavi manifest (its `installDir` keys and title), and enumerate the
//! install dirs under a generic "game folder" root. One level deep only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use savr_core::manifest::Manifest;

use crate::naming::normalize_title;

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
        assert_eq!(
            matcher.match_folder("Hollow Knight").as_deref(),
            Some("Hollow Knight")
        );
        assert_eq!(
            matcher.match_folder("hollowknight").as_deref(),
            Some("Hollow Knight")
        );
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

        let mut names: Vec<String> = scan_folder_root(dir.path())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["Celeste".to_string(), "Hollow Knight".to_string()]
        );
    }
}
