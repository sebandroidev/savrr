//! Structured error returned from every Tauri command.
//!
//! It serializes to `{ "kind": "...", "message": "..." }` so the frontend can
//! branch on `kind` (e.g. show a "start the daemon" hint on `daemon_unreachable`)
//! instead of string-matching. Commands never panic — a missing daemon is an
//! expected, first-class state.

use serde::Serialize;

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum CmdError {
    /// Could not connect to the daemon socket/pipe (daemon not running).
    #[error("the Savr daemon is not running or is unreachable: {0}")]
    DaemonUnreachable(String),
    /// The daemon accepted the request but replied with an error.
    #[error("the daemon reported an error: {0}")]
    Daemon(String),
    /// The daemon replied with a message we did not expect for this request.
    #[error("unexpected daemon response: {0}")]
    Protocol(String),
    /// Low-level IO failure while framing/sending/receiving.
    #[error("IPC transport error: {0}")]
    Io(String),
}
