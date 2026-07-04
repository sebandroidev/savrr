//! Ludusavi-manifest parsing (PRD-02 §1) and path-placeholder resolution
//! (§1.3). We consume a subset of the manifest — unknown fields are ignored,
//! so the upstream schema can grow without breaking us.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::IgnoredAny;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::types::{Os, SaveTag, SaveTarget};

/// Whole manifest: title -> entry.
pub type Manifest = BTreeMap<String, ManifestEntry>;

#[derive(Deserialize, Debug, Clone, Default)]
pub struct ManifestEntry {
    #[serde(default)]
    pub files: BTreeMap<String, FileMeta>,
    /// Names of install directories; values are ignored.
    #[serde(default, rename = "installDir")]
    pub install_dir: BTreeMap<String, IgnoredAny>,
    /// Windows registry keys (HKCU) that hold saves/config.
    #[serde(default)]
    pub registry: BTreeMap<String, FileMeta>,
    #[serde(default)]
    pub steam: Option<Steam>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct FileMeta {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub when: Option<Vec<When>>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct When {
    pub os: Option<String>,
    pub store: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Copy)]
pub struct Steam {
    pub id: u32,
}

impl ManifestEntry {
    /// Flatten this entry's file + registry locations into `SaveTarget`s.
    pub fn save_targets(&self) -> Vec<SaveTarget> {
        let mut out = Vec::new();
        for (glob, meta) in &self.files {
            out.push(SaveTarget {
                glob: glob.clone(),
                tags: meta.tags.iter().filter_map(|t| SaveTag::parse(t)).collect(),
                // `when` is a hint, not a filter (PRD-02 §1.2): take the first os.
                os_hint: meta
                    .when
                    .as_ref()
                    .and_then(|w| w.first())
                    .and_then(|w| w.os.as_deref())
                    .and_then(Os::parse),
                registry: false,
            });
        }
        for (key, meta) in &self.registry {
            out.push(SaveTarget {
                glob: key.clone(),
                tags: meta.tags.iter().filter_map(|t| SaveTag::parse(t)).collect(),
                os_hint: Some(Os::Windows),
                registry: true,
            });
        }
        out
    }
}

pub fn parse(yaml: &str) -> Result<Manifest> {
    serde_yaml_ng::from_str(yaml).map_err(|e| Error::Manifest(e.to_string()))
}

/// Well-known directories used to expand placeholders (PRD-02 §1.3). Populated
/// for the current OS but every field is optional so cross-OS templates
/// resolve where the directory happens to exist.
#[derive(Debug, Clone)]
pub struct Roots {
    pub home: PathBuf,
    pub win_app_data: Option<PathBuf>,
    pub win_local_app_data: Option<PathBuf>,
    pub win_documents: Option<PathBuf>,
    pub xdg_data: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
    pub os_user_name: Option<String>,
    pub os: Os,
}

impl Roots {
    pub fn current() -> Self {
        Roots {
            home: dirs::home_dir().unwrap_or_default(),
            win_app_data: dirs::config_dir(),
            win_local_app_data: dirs::data_local_dir(),
            win_documents: dirs::document_dir(),
            xdg_data: dirs::data_dir(),
            xdg_config: dirs::config_dir(),
            os_user_name: std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .ok(),
            os: Os::current(),
        }
    }
}

/// Resolve a save-path template into a concrete glob pattern. `base` is the
/// game's install dir (for `<base>`), if known. Returns `None` when the
/// template needs a placeholder we can't resolve on this machine — the caller
/// skips that target and tries the game's other targets.
pub fn resolve(template: &str, roots: &Roots, base: Option<&Path>) -> Option<String> {
    let disp = |p: &Path| p.display().to_string();
    let subs: [(&str, Option<String>); 8] = [
        ("<base>", base.map(disp)),
        ("<home>", Some(disp(&roots.home))),
        ("<winAppData>", roots.win_app_data.as_deref().map(disp)),
        (
            "<winLocalAppData>",
            roots.win_local_app_data.as_deref().map(disp),
        ),
        ("<winDocuments>", roots.win_documents.as_deref().map(disp)),
        ("<xdgData>", roots.xdg_data.as_deref().map(disp)),
        ("<xdgConfig>", roots.xdg_config.as_deref().map(disp)),
        ("<osUserName>", roots.os_user_name.clone()),
    ];

    let mut out = template.to_string();
    for (ph, val) in subs {
        if out.contains(ph) {
            match val {
                Some(v) => out = out.replace(ph, &v),
                None => return None,
            }
        }
    }
    // `<storeUserId>` needs the Steam login id, which we don't have in core —
    // leave it for the daemon to fill. If it survived to here, we can't resolve.
    if out.contains('<') {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
An Example Game:
  files:
    "<base>/saves":
      tags: [save]
    "<home>/settings.json":
      when: [{ os: windows }, { os: linux }]
      tags: [config]
  installDir:
    AnExampleGame: {}
  registry:
    "HKEY_CURRENT_USER/Software/An Example Game":
      tags: [save, config]
  steam:
    id: 123
"#;

    #[test]
    fn parses_subset_and_ignores_unknown() {
        let m = parse(SAMPLE).unwrap();
        let e = &m["An Example Game"];
        assert_eq!(e.steam.unwrap().id, 123);
        assert!(e.install_dir.contains_key("AnExampleGame"));
        assert_eq!(e.files.len(), 2);
        assert_eq!(e.registry.len(), 1);

        let targets = e.save_targets();
        assert_eq!(targets.len(), 3);
        let reg = targets.iter().find(|t| t.registry).unwrap();
        assert_eq!(reg.os_hint, Some(Os::Windows));
        assert!(reg.tags.contains(&SaveTag::Save) && reg.tags.contains(&SaveTag::Config));
    }

    #[test]
    fn resolves_and_skips_unknown_placeholder() {
        let roots = Roots {
            home: PathBuf::from("/home/me"),
            win_app_data: None,
            win_local_app_data: None,
            win_documents: None,
            xdg_data: Some(PathBuf::from("/home/me/.local/share")),
            xdg_config: None,
            os_user_name: Some("me".into()),
            os: Os::Linux,
        };
        assert_eq!(
            resolve("<base>/saves", &roots, Some(Path::new("/games/foo"))),
            Some("/games/foo/saves".into())
        );
        assert_eq!(
            resolve("<home>/settings.json", &roots, None),
            Some("/home/me/settings.json".into())
        );
        // No base supplied -> can't resolve <base>.
        assert_eq!(resolve("<base>/x", &roots, None), None);
        // Unknown placeholder we don't handle -> skip.
        assert_eq!(resolve("<storeUserId>/x", &roots, None), None);
    }
}
