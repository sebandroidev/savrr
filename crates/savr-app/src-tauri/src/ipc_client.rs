//! Thin IPC client to the Savr daemon.
//!
//! Transport is a Unix domain socket (Linux/macOS) or a named pipe (Windows),
//! abstracted by `interprocess`. Each call is one short-lived connection that
//! writes a single [`GuiRequest`] frame and reads a single [`DaemonMsg`] frame
//! back, using the length-prefixed JSON codec from `savr_core::ipc`.
//!
//! The connection is intentionally per-request rather than pooled: the GUI is
//! not hot-path and this keeps state trivial. If a streaming event feed is
//! added later (live `DetectionEvent`s / `ConflictRaised`), that would be a
//! separate long-lived connection.
//
// ponytail: per-request connect (no pooling, no reconnect/backoff). Ceiling:
// fine for click-driven GUI traffic. Upgrade path: hold one long-lived
// connection in Tauri state + a background reader that emits Tauri events when
// the live-feed milestone lands.

use interprocess::local_socket::tokio::{prelude::*, Stream};
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;
use savr_core::ipc::{read_frame, write_frame, DaemonMsg, GuiRequest};

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

    let reply: Option<DaemonMsg> = read_frame(&mut sock)
        .await
        .map_err(|e| CmdError::Io(e.to_string()))?;

    match reply {
        Some(DaemonMsg::Error { message }) => Err(CmdError::Daemon(message)),
        Some(msg) => Ok(msg),
        None => Err(CmdError::DaemonUnreachable(
            "daemon closed the connection without responding".to_owned(),
        )),
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
