//! `savr-core` — shared types, manifest parsing, and snapshot/diff for the Savr
//! save-sync suite. Compiled into the daemon, GUI, and server so the wire
//! format can never drift between tiers (PRD-01 §3).

pub mod archive;
pub mod error;
pub mod hash;
pub mod ipc;
pub mod manifest;
pub mod protocol;
pub mod snapshot;
pub mod types;

pub use error::{Error, Result};
pub use hash::Blake3Hash;
pub use types::*;
