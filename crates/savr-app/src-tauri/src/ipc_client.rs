//! Thin IPC client to the Savr daemon.
//!
//! Transport is a Unix domain socket (Linux/macOS) or a named pipe (Windows),
//! abstracted by `interprocess`. Each call is one short-lived connection that
//! writes a single [`GuiRequest`] frame and reads a single [`DaemonMsg`] frame
//! back, using the length-prefixed JSON codec from `savr_core::ipc`.
//!
//! Request/response ([`request`]) is per-connection rather than pooled: the GUI
//! is not hot-path and this keeps state trivial. Separately, [`run_event_subscription`]
//! holds one long-lived connection that reads the `DetectionEvent`s the daemon
//! pushes and shows them as OS toasts.

use std::collections::HashMap;
use std::time::Duration;

use interprocess::local_socket::tokio::{prelude::*, Stream};
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use savr_core::ipc::{read_frame, write_frame, DaemonMsg, DetectionEvent, GuiRequest};
use savr_core::GameId;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

use crate::error::CmdError;

// --- Daemon endpoint: the single source of truth for where the daemon listens.
//
// The endpoint string comes from the one shared resolver in savr-core, so the
// GUI and the daemon can never disagree (unix: a filesystem path, windows: the
// `savr-daemon` pipe name). We only choose the interprocess name *type* per OS.
use savr_core::ipc::default_ipc_endpoint;

/// Open a fresh connection to the daemon.
async fn connect() -> Result<Stream, CmdError> {
    let endpoint = default_ipc_endpoint();
    #[cfg(unix)]
    let name = endpoint
        .as_str()
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| CmdError::Io(e.to_string()))?;
    #[cfg(windows)]
    let name = endpoint
        .as_str()
        .to_ns_name::<GenericNamespaced>()
        .map_err(|e| CmdError::Io(e.to_string()))?;
    Stream::connect(name)
        .await
        .map_err(|e| CmdError::DaemonUnreachable(e.to_string()))
}

/// True if a daemon is already accepting connections on the shared endpoint.
/// The supervisor uses this to defer to an existing daemon instead of spawning
/// a second one that would fight over the same socket + database.
pub async fn is_daemon_running() -> bool {
    connect().await.is_ok()
}

/// Send one request, read one reply. A `DaemonMsg::Error` is surfaced as a
/// [`CmdError::Daemon`]; every other variant is handed back to the caller to
/// match against what it expects for that request.
pub async fn request(req: GuiRequest) -> Result<DaemonMsg, CmdError> {
    let conn = connect().await?;
    // interprocess tokio `Stream` implements AsyncRead + AsyncWrite through a
    // shared `&Stream`, so a single connection handles the write-then-read
    // exchange without an explicit split.
    let mut sock = &conn;

    write_frame(&mut sock, &req)
        .await
        .map_err(|e| CmdError::Io(e.to_string()))?;

    // Every connection also carries the daemon's live event stream, so a pushed
    // `Event` frame can land between our request and its reply. Skip events until
    // the actual reply arrives (replies are 1:1 with requests).
    loop {
        let frame: Option<DaemonMsg> = read_frame(&mut sock)
            .await
            .map_err(|e| CmdError::Io(e.to_string()))?;
        match frame {
            Some(DaemonMsg::Event(_)) => continue,
            Some(DaemonMsg::Error { message }) => return Err(CmdError::Daemon(message)),
            Some(msg) => return Ok(msg),
            None => {
                return Err(CmdError::DaemonUnreachable(
                    "daemon closed the connection without responding".to_owned(),
                ))
            }
        }
    }
}

// --- Live event subscription: a single long-lived connection that reads the
// DetectionEvents the daemon pushes and turns the notification-worthy ones into
// OS toasts. Shown from Rust (not the webview) so they fire even while the app
// is hidden to the tray. This is the "live-feed" upgrade the request() doc
// comment anticipated.

/// Read pushed daemon events forever, toasting the interesting ones. Reconnects
/// with a short backoff so it survives daemon restarts (the app respawns the
/// sidecar). Never returns.
pub async fn run_event_subscription(app: AppHandle) {
    let mut titles: HashMap<GameId, String> = HashMap::new();
    loop {
        if let Err(e) = subscribe_once(&app, &mut titles).await {
            tracing::debug!("event subscription dropped, reconnecting: {e}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// One connection's lifetime: read frames until the daemon closes the pipe.
/// The daemon forwards every event as `DaemonMsg::Event` on connect with no
/// request needed, so we only read — we never write on this socket.
async fn subscribe_once(
    app: &AppHandle,
    titles: &mut HashMap<GameId, String>,
) -> Result<(), CmdError> {
    let conn = connect().await?;
    let mut sock = &conn;
    loop {
        let msg: Option<DaemonMsg> = read_frame(&mut sock)
            .await
            .map_err(|e| CmdError::Io(e.to_string()))?;
        match msg {
            None => return Ok(()), // clean EOF: daemon exited -> reconnect
            Some(DaemonMsg::Event(event)) => {
                if let Some((title, body)) = toast_for(event, titles).await {
                    let _ = app.notification().builder().title(title).body(body).show();
                }
            }
            Some(_) => {} // no requests sent here; ignore any stray reply
        }
    }
}

/// Rebuild the game_id -> title map from the daemon's current catalog so toasts
/// can name the game. Best-effort: a failed lookup just leaves the cache as-is.
async fn refresh_titles(titles: &mut HashMap<GameId, String>) {
    if let Ok(DaemonMsg::Games(games)) = request(GuiRequest::ListGames).await {
        titles.clear();
        for g in games {
            titles.insert(g.id, g.title);
        }
    }
}

/// A game's display title. Toasts are rare, and the daemon's daily manifest
/// refresh can rename a game while keeping its `game_id` (Steam folder name ->
/// canonical manifest title), so refresh every time rather than trust a
/// possibly-stale cache hit. `refresh_titles` only clears + rebuilds on a
/// successful `ListGames`, so a transient failure keeps the last-known names.
async fn title_for(game_id: GameId, titles: &mut HashMap<GameId, String>) -> String {
    refresh_titles(titles).await;
    titles
        .get(&game_id)
        .cloned()
        .unwrap_or_else(|| "your game".to_string())
}

/// Map an event to a `(title, body)` toast, or `None` if it's not worth one.
async fn toast_for(
    event: DetectionEvent,
    titles: &mut HashMap<GameId, String>,
) -> Option<(String, String)> {
    match event {
        DetectionEvent::GameStarted { game_id, .. } => {
            let t = title_for(game_id, titles).await;
            Some((
                format!("Now watching {t}"),
                "Savr backs up your save when you close the game.".to_string(),
            ))
        }
        DetectionEvent::GameStopped { game_id, .. } => {
            let t = title_for(game_id, titles).await;
            Some((
                format!("{t} closed"),
                "Checking for new save data…".to_string(),
            ))
        }
        DetectionEvent::BackupCompleted { game_id } => {
            let t = title_for(game_id, titles).await;
            Some((
                format!("{t} backed up"),
                "Your latest save is safe with Savr.".to_string(),
            ))
        }
        DetectionEvent::BackupConflict { game_id } => {
            let t = title_for(game_id, titles).await;
            Some((
                "Sync conflict".to_string(),
                format!("{t} — open Savr to choose which save to keep."),
            ))
        }
        DetectionEvent::SaveAvailable { game_id } => {
            let t = title_for(game_id, titles).await;
            Some((
                "New save available".to_string(),
                format!("{t} — open Savr to download it."),
            ))
        }
        DetectionEvent::ManualBackupRequested { .. } | DetectionEvent::SaveDirChanged { .. } => {
            None
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn endpoint_targets_daemon_sock() {
        // The GUI resolves the same endpoint the daemon binds. On unix, with or
        // without a runtime dir, it ends at the daemon socket file name.
        let p = default_ipc_endpoint();
        assert!(p.ends_with("daemon.sock"), "unexpected socket path: {p}");
    }
}
