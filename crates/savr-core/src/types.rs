//! Core wire types (PRD-05 §1). These structs are the JSON bodies on the REST
//! API, the SQL row payloads (`files_json`, `data_json`), and the daemon IPC
//! frames — one definition, three surfaces.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hash::Blake3Hash;

pub type GameId = Uuid;
pub type VersionId = Uuid;
pub type DeviceId = Uuid;
pub type AccountId = Uuid;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    Linux,
    Macos,
}

impl Os {
    pub fn current() -> Self {
        #[cfg(target_os = "windows")]
        {
            Os::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Os::Macos
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            Os::Linux
        }
    }

    /// Parse a Ludusavi-manifest `os` value ("windows" | "linux" | "mac").
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "windows" => Some(Os::Windows),
            "linux" => Some(Os::Linux),
            "mac" | "macos" => Some(Os::Macos),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SaveTag {
    Save,
    Config,
}

impl SaveTag {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "save" => Some(SaveTag::Save),
            "config" => Some(SaveTag::Config),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameSource {
    /// Matched a Ludusavi manifest entry — we know where the saves live.
    Manifest,
    /// Installed Steam game with no manifest match yet; detectable by its
    /// executables, save paths learned on first play.
    Steam,
    /// User-added by hand.
    Custom,
}

/// A save location template, pre-resolution (still holds `<placeholders>`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SaveTarget {
    pub glob: String,
    pub tags: Vec<SaveTag>,
    pub os_hint: Option<Os>,
    /// Windows registry key vs filesystem path.
    pub registry: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Game {
    pub id: GameId,
    pub title: String,
    pub source: GameSource,
    pub steam_appid: Option<u32>,
    pub save_targets: Vec<SaveTarget>,
    // Detection/play stats, overlaid by the daemon's ListGames from its local
    // `play_stats` table + live running set. Defaulted so every other producer
    // of a Game (and any older payload) stays valid without setting them.
    /// True while a process from this game's install dir is running right now.
    #[serde(default)]
    pub running: bool,
    /// When the game was last seen starting (RFC3339 UTC), if ever.
    #[serde(default)]
    pub last_played: Option<DateTime<Utc>>,
    /// Duration of the most recently finished play session, in seconds.
    #[serde(default)]
    pub last_session_secs: Option<i64>,
    /// Accumulated play time across all finished sessions, in seconds.
    #[serde(default)]
    pub total_secs: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub rel_path: String,
    pub size: u64,
    pub mtime: i64,
    pub hash: Blake3Hash,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VersionKind {
    Full,
    Differential,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Version {
    pub id: VersionId,
    pub game_id: GameId,
    pub device_id: DeviceId,
    pub parent: Option<VersionId>,
    pub kind: VersionKind,
    pub files: Vec<FileEntry>,
    pub blob_hash: Blake3Hash,
    pub bytes: u64,
    /// Server-assigned, monotonic per (account, game). 0 before assignment.
    pub seq: u64,
    pub created_at: DateTime<Utc>,
}

/// Request body for `POST /games/{id}/versions` (PRD-04 §2). The server
/// assigns `id`, `seq`, and `created_at`; the client supplies the rest.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateVersion {
    pub parent: Option<VersionId>,
    pub kind: VersionKind,
    pub files: Vec<FileEntry>,
    pub blob_hash: Blake3Hash,
    pub bytes: u64,
    pub device_id: DeviceId,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
    pub os: Os,
    pub last_seen: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PathOverride {
    pub game_id: GameId,
    pub globs: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    #[default]
    Manual,
    LatestWins,
    TheirsWins,
    MineWins,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoPullPolicy {
    #[default]
    Ask,
    Auto,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    pub full: u32,
    pub diff_per_full: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            full: 5,
            diff_per_full: 10,
        }
    }
}

/// Account-level config, synced to every device (PRD-04 `/config`).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SyncedConfig {
    pub tag: String,
    pub custom_games: Vec<Game>,
    pub overrides: Vec<PathOverride>,
    pub conflict_policy: ConflictPolicy,
    pub autopull_policy: AutoPullPolicy,
    pub retention: Retention,
}
