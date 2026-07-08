//! `savr-daemon` — the headless always-on save-sync service (PRD-07 §2).
//!
//! Detects games starting/stopping (PRD-02), snapshots + uploads their saves
//! (PRD-03), keeps its own local state (PRD-05 §3), and speaks to the GUI over
//! a length-prefixed IPC socket (PRD-05 §4). It talks to `savr-server` purely
//! through `savr_core::protocol` types over HTTPS REST + WebSocket — it never
//! imports the server crate, so the wire format can't drift.
//!
//! Everything lives in a library so integration tests can drive the engine and
//! the IPC frame dispatcher in-process; `main.rs` is a thin task launcher.

pub mod autostart;
pub mod backup;
pub mod client;
pub mod config;
pub mod detection;
pub mod engine;
pub mod ipc;
pub mod manifest_sync;
pub mod naming;
pub mod paths;
pub mod restore;
pub mod secrets;
pub mod state;
pub mod tray;
pub mod ws;

pub use config::DaemonConfig;
pub use engine::Engine;
pub use state::LocalState;

/// Where the daemon listens for GUI connections (PRD-05 §4). Delegates to the
/// shared resolver in `savr-core` so the daemon and the GUI cannot disagree on
/// the path. Honors the `SAVR_IPC_PATH` override.
pub fn ipc_path() -> String {
    savr_core::ipc::default_ipc_endpoint()
}
