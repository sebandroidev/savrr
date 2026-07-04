//! `daemon.toml` loader (PRD-07 §2). Server URL, poll cadence, sync policies,
//! and per-game overrides — with environment-variable overrides layered on top
//! so a service manager or CI can tweak behavior without editing the file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use savr_core::{AutoPullPolicy, ConflictPolicy};

/// Default active poll interval (a known game is running): PRD-02 §3.1 "2–5s".
const DEFAULT_POLL_ACTIVE_MS: u64 = 3_000;
/// Default idle poll interval (nothing interesting running): back off to save
/// CPU — PRD-02 §3.1 "10–15s".
const DEFAULT_POLL_IDLE_MS: u64 = 12_000;
/// Default settle window after a game exits before we read its saves so the
/// game finishes flushing (PRD-02 §3.1 "3–8s").
const DEFAULT_SETTLE_MS: u64 = 5_000;
/// Take a fresh full every N differentials so restore chains stay short
/// (PRD-03 §2).
const DEFAULT_FULL_EVERY: u32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Base server URL, e.g. `https://nas.local:8080`. The REST client appends
    /// `/api/v1`; the WS client swaps the scheme and appends `/ws`.
    pub server_url: String,
    /// Poll interval while a known game is running.
    pub poll_interval_ms: u64,
    /// Poll interval while idle (adaptive back-off, PRD-02 §3.1).
    pub poll_idle_interval_ms: u64,
    /// Debounce after a game exits before snapshotting (PRD-02 §3.1).
    pub settle_ms: u64,
    /// Auto-pull policy for incoming versions (PRD-03 §5). Also a default that
    /// the synced config can override at runtime.
    pub autopull: AutoPullPolicy,
    /// Conflict policy default (PRD-03 §4).
    pub conflict_policy: ConflictPolicy,
    /// Force a full every N differentials (PRD-03 §2).
    pub full_every: u32,
    /// Directory the Ludusavi manifest is cached in (PRD-02 §1.1).
    pub manifest_dir: Option<PathBuf>,
    /// Per-game overrides keyed by game title (or steam appid as a string).
    #[serde(default)]
    pub games: BTreeMap<String, GameOverride>,
}

/// Per-game knobs (PRD-02 §1.4 override redirect + policy pinning).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameOverride {
    /// Explicit save globs replacing the manifest's (with placeholders).
    pub globs: Vec<String>,
    /// Pin a conflict policy for just this game.
    pub conflict_policy: Option<ConflictPolicy>,
    /// Pin an autopull policy for just this game.
    pub autopull: Option<AutoPullPolicy>,
    /// Never watch/back up this game.
    pub ignore: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            server_url: "http://localhost:8080".to_string(),
            poll_interval_ms: DEFAULT_POLL_ACTIVE_MS,
            poll_idle_interval_ms: DEFAULT_POLL_IDLE_MS,
            settle_ms: DEFAULT_SETTLE_MS,
            autopull: AutoPullPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            full_every: DEFAULT_FULL_EVERY,
            manifest_dir: None,
            games: BTreeMap::new(),
        }
    }
}

impl DaemonConfig {
    /// Default config file location: `~/.config/savr/daemon.toml` (PRD-07 §2),
    /// overridable with `SAVR_CONFIG`.
    pub fn default_path() -> PathBuf {
        if let Ok(p) = std::env::var("SAVR_CONFIG") {
            return PathBuf::from(p);
        }
        config_root().join("daemon.toml")
    }

    /// Load config from `path` (missing file → defaults), then apply env
    /// overrides. Never fails on a missing file — a fresh install just runs on
    /// defaults until the GUI writes one.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DaemonConfig::default(),
            Err(e) => return Err(e.into()),
        };
        cfg.apply_env();
        Ok(cfg)
    }

    /// Convenience: load from the default path.
    pub fn load_default() -> anyhow::Result<Self> {
        Self::load(&Self::default_path())
    }

    /// Overlay environment variables (highest precedence). Only the fields a
    /// service manager realistically wants to override are wired.
    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("SAVR_SERVER_URL") {
            self.server_url = v;
        }
        if let Some(v) = env_parse::<u64>("SAVR_POLL_INTERVAL_MS") {
            self.poll_interval_ms = v;
        }
        if let Some(v) = env_parse::<u64>("SAVR_POLL_IDLE_INTERVAL_MS") {
            self.poll_idle_interval_ms = v;
        }
        if let Some(v) = env_parse::<u64>("SAVR_SETTLE_MS") {
            self.settle_ms = v;
        }
        if let Ok(v) = std::env::var("SAVR_MANIFEST_DIR") {
            self.manifest_dir = Some(PathBuf::from(v));
        }
    }

    /// The manifest cache dir, honoring the config/env override, else the
    /// per-user data dir.
    pub fn manifest_dir(&self) -> PathBuf {
        self.manifest_dir
            .clone()
            .unwrap_or_else(|| data_root().join("manifests"))
    }

    /// Effective conflict policy for a game, applying any per-game pin.
    pub fn conflict_policy_for(&self, key: &str) -> ConflictPolicy {
        self.games
            .get(key)
            .and_then(|g| g.conflict_policy)
            .unwrap_or(self.conflict_policy)
    }
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

/// `~/.config/savr` (or platform equivalent), overridable via `SAVR_STATE_DIR`.
pub fn config_root() -> PathBuf {
    if let Ok(p) = std::env::var("SAVR_STATE_DIR") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("savr")
}

/// Per-user data dir for the local state DB + manifest cache.
pub fn data_root() -> PathBuf {
    if let Ok(p) = std::env::var("SAVR_STATE_DIR") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("savr")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_toml_and_defaults_fill_gaps() {
        let toml = r#"
            server_url = "https://nas.local:9000"
            poll_interval_ms = 2500

            [games."Celeste"]
            ignore = true
            conflict_policy = "mine_wins"
        "#;
        let cfg: DaemonConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.server_url, "https://nas.local:9000");
        assert_eq!(cfg.poll_interval_ms, 2500);
        // Unspecified fields fall back to defaults.
        assert_eq!(cfg.settle_ms, DEFAULT_SETTLE_MS);
        assert!(cfg.games["Celeste"].ignore);
        assert_eq!(cfg.conflict_policy_for("Celeste"), ConflictPolicy::MineWins);
        // Unknown game → global default.
        assert_eq!(cfg.conflict_policy_for("Hades"), ConflictPolicy::Manual);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = DaemonConfig::load(Path::new("/nonexistent/savr/daemon.toml")).unwrap();
        assert_eq!(cfg.poll_interval_ms, DEFAULT_POLL_ACTIVE_MS);
    }
}
