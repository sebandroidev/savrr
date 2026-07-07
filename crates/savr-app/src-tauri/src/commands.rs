//! Tauri commands — thin, one-to-one wrappers over the daemon IPC requests.
//!
//! Each command builds a [`GuiRequest`], sends it via [`crate::ipc_client`],
//! and narrows the [`DaemonMsg`] reply to the payload the frontend expects. A
//! mismatched reply becomes a [`CmdError::Protocol`]; a missing daemon becomes
//! [`CmdError::DaemonUnreachable`]. Nothing here panics.

use savr_core::ipc::{DaemonMsg, DaemonStatus, GuiRequest, ResolveChoice, Root, RootSpec};
use savr_core::types::{Game, GameId, SyncedConfig, Version, VersionId};
use uuid::Uuid;

use crate::error::CmdError;
use crate::ipc_client::request;

/// Assert the daemon acknowledged a mutation with `Ok`.
fn expect_ok(msg: DaemonMsg) -> Result<(), CmdError> {
    match msg {
        DaemonMsg::Ok => Ok(()),
        other => Err(CmdError::Protocol(format!("expected Ok, got {other:?}"))),
    }
}

#[tauri::command]
pub async fn list_games() -> Result<Vec<Game>, CmdError> {
    match request(GuiRequest::ListGames).await? {
        DaemonMsg::Games(games) => Ok(games),
        other => Err(CmdError::Protocol(format!("expected Games, got {other:?}"))),
    }
}

#[tauri::command]
pub async fn list_roots() -> Result<Vec<Root>, CmdError> {
    match request(GuiRequest::ListRoots).await? {
        DaemonMsg::Roots(roots) => Ok(roots),
        other => Err(CmdError::Protocol(format!("expected Roots, got {other:?}"))),
    }
}

#[tauri::command]
pub async fn add_root(spec: RootSpec) -> Result<(), CmdError> {
    expect_ok(request(GuiRequest::AddRoot(spec)).await?)
}

#[tauri::command]
pub async fn remove_root(id: Uuid) -> Result<(), CmdError> {
    expect_ok(request(GuiRequest::RemoveRoot { id }).await?)
}

#[tauri::command]
pub async fn backup_now(game_id: GameId) -> Result<(), CmdError> {
    match request(GuiRequest::BackupNow { game_id }).await? {
        // A manual backup whose save diverged from the server head produces a
        // conflict — an expected outcome, not a command failure. It's surfaced
        // in the Conflicts view; don't turn it into a scary "protocol error".
        DaemonMsg::Ok | DaemonMsg::ConflictRaised { .. } => Ok(()),
        other => Err(CmdError::Protocol(format!("expected Ok, got {other:?}"))),
    }
}

#[tauri::command]
pub async fn list_versions(game_id: GameId) -> Result<Vec<Version>, CmdError> {
    match request(GuiRequest::ListVersions { game_id }).await? {
        DaemonMsg::Versions(versions) => Ok(versions),
        other => Err(CmdError::Protocol(format!(
            "expected Versions, got {other:?}"
        ))),
    }
}

#[tauri::command]
pub async fn restore(game_id: GameId, version_id: VersionId) -> Result<(), CmdError> {
    expect_ok(
        request(GuiRequest::Restore {
            game_id,
            version_id,
        })
        .await?,
    )
}

#[tauri::command]
pub async fn resolve_conflict(game_id: GameId, choice: ResolveChoice) -> Result<(), CmdError> {
    expect_ok(request(GuiRequest::ResolveConflict { game_id, choice }).await?)
}

#[tauri::command]
pub async fn get_status() -> Result<DaemonStatus, CmdError> {
    match request(GuiRequest::GetStatus).await? {
        DaemonMsg::Status(status) => Ok(status),
        other => Err(CmdError::Protocol(format!(
            "expected Status, got {other:?}"
        ))),
    }
}

/// Register (or unregister) the daemon to start on Windows login (headless).
#[tauri::command]
pub async fn set_autostart(enabled: bool) -> Result<(), CmdError> {
    expect_ok(request(GuiRequest::SetAutostart(enabled)).await?)
}

#[tauri::command]
pub async fn get_config() -> Result<SyncedConfig, CmdError> {
    match request(GuiRequest::GetConfig).await? {
        DaemonMsg::Config(config) => Ok(*config),
        other => Err(CmdError::Protocol(format!(
            "expected Config, got {other:?}"
        ))),
    }
}

#[tauri::command]
pub async fn update_config(config: SyncedConfig) -> Result<(), CmdError> {
    expect_ok(request(GuiRequest::UpdateConfig(Box::new(config))).await?)
}

#[tauri::command]
pub async fn enter_learn_mode(game_id: GameId) -> Result<(), CmdError> {
    expect_ok(request(GuiRequest::EnterLearnMode { game_id }).await?)
}

#[tauri::command]
pub async fn pair_device(
    server_url: String,
    code: String,
    device_name: String,
) -> Result<Uuid, CmdError> {
    match request(GuiRequest::PairDevice {
        server_url,
        code,
        device_name,
    })
    .await?
    {
        DaemonMsg::Paired { device_id } => Ok(device_id),
        other => Err(CmdError::Protocol(format!(
            "expected Paired, got {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_choice_matches_frontend_wire_form() {
        // The Svelte UI sends these snake_case strings; verify the contract.
        let mine: ResolveChoice = serde_json::from_str("\"keep_mine\"").unwrap();
        let theirs: ResolveChoice = serde_json::from_str("\"keep_theirs\"").unwrap();
        let both: ResolveChoice = serde_json::from_str("\"keep_both\"").unwrap();
        assert_eq!(mine, ResolveChoice::KeepMine);
        assert_eq!(theirs, ResolveChoice::KeepTheirs);
        assert_eq!(both, ResolveChoice::KeepBoth);
    }

    #[test]
    fn cmd_error_serializes_with_kind_and_message() {
        let err = CmdError::DaemonUnreachable("boom".to_owned());
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "daemon_unreachable");
        assert_eq!(json["message"], "boom");
    }
}
