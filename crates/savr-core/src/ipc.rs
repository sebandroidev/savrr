//! Daemon ↔ GUI local IPC (PRD-05 §4): message enums + a length-prefixed JSON
//! frame codec. Transport is a unix socket / named pipe owned by the daemon;
//! this module only defines what flows over it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::{Game, GameId, SyncedConfig, Version, VersionId};

// ---- shared value types ----

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    Steam,
    Drive,
    Emulator,
    Launcher,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RootSpec {
    pub kind: RootKind,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Root {
    pub id: Uuid,
    pub kind: RootKind,
    pub path: String,
}

/// A hand-registered game not found in any Steam library.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CustomGameSpec {
    pub title: String,
    pub install_path: Option<String>,
    pub save_root: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolveChoice {
    KeepMine,
    KeepTheirs,
    KeepBoth,
}

/// Daemon health for the GUI dashboard — the "prove it's tiny" story (G5,
/// PRD-07 §6).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DaemonStatus {
    pub version: String,
    pub uptime_s: u64,
    pub rss_bytes: u64,
    pub watched_games: u32,
    pub server_connected: bool,
    pub last_backup_at: Option<DateTime<Utc>>,
    pub pending_outbox: u32,
    /// Whether the daemon is registered to start on OS login (Windows only;
    /// always false elsewhere). Defaulted so older payloads still deserialize.
    #[serde(default)]
    pub autostart_enabled: bool,
}

/// Engine events streamed to the GUI live feed (PRD-02 §5): raw detection edges
/// plus the backup/sync outcomes the app turns into desktop toasts. Toasts are
/// the app's job, never the daemon's — the daemon also runs headless as a
/// server, where there is no desktop to notify.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DetectionEvent {
    GameStarted {
        game_id: GameId,
        pid: u32,
        at: DateTime<Utc>,
    },
    GameStopped {
        game_id: GameId,
        at: DateTime<Utc>,
    },
    ManualBackupRequested {
        game_id: GameId,
    },
    SaveDirChanged {
        game_id: GameId,
    },
    /// A save was backed up (usually on game exit) — the app confirms with a toast.
    BackupCompleted {
        game_id: GameId,
    },
    /// A backup diverged from the server head; the user must pick a save to keep.
    BackupConflict {
        game_id: GameId,
    },
    /// A newer save arrived from another device and was not auto-pulled.
    SaveAvailable {
        game_id: GameId,
    },
    /// The games catalog was (re)built — startup, manifest refresh, or a roots
    /// change. The GUI reloads its list on this: `refresh_games` runs after a
    /// slow manifest fetch, so an initial GUI query can otherwise see an empty
    /// catalog and never re-ask.
    CatalogUpdated,
}

// ---- request / response enums ----

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuiRequest {
    ListGames,
    ListRoots,
    AddRoot(RootSpec),
    RemoveRoot {
        id: Uuid,
    },
    BackupNow {
        game_id: GameId,
    },
    ListVersions {
        game_id: GameId,
    },
    Restore {
        game_id: GameId,
        version_id: VersionId,
    },
    ResolveConflict {
        game_id: GameId,
        choice: ResolveChoice,
    },
    GetStatus,
    GetConfig,
    UpdateConfig(Box<SyncedConfig>),
    EnterLearnMode {
        game_id: GameId,
    },
    PairDevice {
        server_url: String,
        code: String,
        device_name: String,
    },
    /// Register (or unregister) the daemon to start on OS login. Windows only.
    /// A struct variant (not `SetAutostart(bool)`): serde's internal tagging
    /// can't serialize a newtype variant wrapping a bare primitive.
    SetAutostart {
        enabled: bool,
    },
    /// Register a game not found in any Steam library.
    AddCustomGame {
        spec: CustomGameSpec,
    },
    /// Remove a hand-registered game by its title.
    RemoveCustomGame {
        title: String,
    },
    /// Fetch the tail of the daemon's log file for the Developer view.
    GetLogs {
        max_lines: usize,
    },
    /// Ask the daemon to shut down. The app sends this before an update relaunch
    /// so whatever daemon is listening — a bundled sidecar or a login-started
    /// headless one the app merely adopted — exits and frees the socket for the
    /// fresh instance, instead of a stale binary surviving the update.
    Shutdown,
}

// Adjacently tagged (`content = "data"`), not internally tagged: the newtype
// variants below wrap sequences (`Vec<Game>`, `Vec<Version>`, `Vec<Root>`), and
// serde cannot serialize an *internally* tagged newtype whose payload is a
// sequence. Adjacent tagging nests the payload under "data" so all variants —
// sequences, structs, and units — round-trip.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DaemonMsg {
    Games(Vec<Game>),
    Versions(Vec<Version>),
    Roots(Vec<Root>),
    Logs(Vec<String>),
    Status(DaemonStatus),
    Config(Box<SyncedConfig>),
    Event(DetectionEvent),
    ConflictRaised {
        game_id: GameId,
        tips: Vec<VersionId>,
    },
    Paired {
        device_id: Uuid,
    },
    Ok,
    Error {
        message: String,
    },
}

// ---- frame codec (feature = "ipc") ----
//
// Frame = 4-byte little-endian u32 length + that many bytes of JSON.

/// Upper bound on a frame; a version list for a huge history stays well under.
pub const MAX_FRAME: u32 = 32 * 1024 * 1024;

/// The daemon's well-known IPC endpoint, shared by the daemon (which binds it)
/// and the GUI (which connects to it) so the two can never drift (PRD-05 §4).
/// `SAVR_IPC_PATH` overrides it (tests, multiple instances).
///
/// - unix: a filesystem path. `$XDG_RUNTIME_DIR/savr/daemon.sock` when a runtime
///   dir exists (the correct user-private location), else `/tmp/savr-daemon.sock`.
///   The caller binds/connects it with interprocess `GenericFilePath`. The
///   daemon must create the parent directory before binding.
/// - windows: the pipe *name* `savr-daemon`, used with `GenericNamespaced`
///   (interprocess maps it to `\\.\pipe\savr-daemon`).
pub fn default_ipc_endpoint() -> String {
    if let Ok(p) = std::env::var("SAVR_IPC_PATH") {
        if !p.is_empty() {
            return p;
        }
    }
    #[cfg(windows)]
    {
        "savr-daemon".to_string()
    }
    #[cfg(not(windows))]
    {
        match dirs::runtime_dir() {
            Some(dir) => dir
                .join("savr")
                .join("daemon.sock")
                .to_string_lossy()
                .into_owned(),
            None => "/tmp/savr-daemon.sock".to_string(),
        }
    }
}

pub fn encode_frame<T: Serialize>(msg: &T) -> std::io::Result<Vec<u8>> {
    let body = serde_json::to_vec(msg)?;
    let len = u32::try_from(body.len()).map_err(|_| std::io::Error::other("frame too large"))?;
    if len > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(feature = "ipc")]
pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
    T: Serialize,
{
    use tokio::io::AsyncWriteExt;
    let frame = encode_frame(msg)?;
    w.write_all(&frame).await?;
    w.flush().await
}

/// Read one frame; `Ok(None)` on clean EOF at a frame boundary.
#[cfg(feature = "ipc")]
pub async fn read_frame<R, T>(r: &mut R) -> std::io::Result<Option<T>>
where
    R: tokio::io::AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(std::io::Error::other("frame exceeds MAX_FRAME"));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

#[cfg(all(test, feature = "ipc"))]
mod tests {
    use super::*;

    #[test]
    fn daemon_msg_vec_variants_roundtrip() {
        // Regression: an internally-tagged newtype wrapping a sequence cannot be
        // serialized. DaemonMsg::{Games,Versions,Roots} must survive the wire,
        // which the adjacent tag (`content = "data"`) guarantees.
        let msg = DaemonMsg::Roots(vec![]);
        let frame = encode_frame(&msg).expect("Vec-newtype variant must encode");
        let v: serde_json::Value = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(v["type"], "roots");
        assert!(v["data"].is_array());
        // And it round-trips back to the same variant.
        let back: DaemonMsg = serde_json::from_slice(&frame[4..]).unwrap();
        assert!(matches!(back, DaemonMsg::Roots(_)));
    }

    #[tokio::test]
    async fn frame_roundtrip_and_eof() {
        let req = GuiRequest::BackupNow {
            game_id: Uuid::nil(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let decoded: GuiRequest = read_frame(&mut cursor).await.unwrap().unwrap();
        assert!(matches!(decoded, GuiRequest::BackupNow { .. }));
        // Clean EOF at frame boundary -> None, not an error.
        let eof: Option<GuiRequest> = read_frame(&mut cursor).await.unwrap();
        assert!(eof.is_none());
    }

    #[test]
    fn set_autostart_encodes() {
        // Regression: an internally-tagged newtype wrapping a bare primitive
        // (`SetAutostart(bool)`) fails to serialize — it must be a struct variant.
        let req = GuiRequest::SetAutostart { enabled: true };
        let frame = encode_frame(&req).expect("SetAutostart must encode");
        let back: GuiRequest = serde_json::from_slice(&frame[4..]).unwrap();
        assert!(matches!(back, GuiRequest::SetAutostart { enabled: true }));
    }

    #[test]
    fn shutdown_encodes() {
        let frame = encode_frame(&GuiRequest::Shutdown).expect("Shutdown must encode");
        let back: GuiRequest = serde_json::from_slice(&frame[4..]).unwrap();
        assert!(matches!(back, GuiRequest::Shutdown));
    }

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

    #[test]
    fn get_logs_and_logs_reply_encode() {
        let frame = encode_frame(&GuiRequest::GetLogs { max_lines: 500 }).unwrap();
        let back: GuiRequest = serde_json::from_slice(&frame[4..]).unwrap();
        assert!(matches!(back, GuiRequest::GetLogs { max_lines: 500 }));

        let frame = encode_frame(&DaemonMsg::Logs(vec!["a".into(), "b".into()])).unwrap();
        let back: DaemonMsg = serde_json::from_slice(&frame[4..]).unwrap();
        assert!(matches!(back, DaemonMsg::Logs(v) if v.len() == 2));
    }
}
