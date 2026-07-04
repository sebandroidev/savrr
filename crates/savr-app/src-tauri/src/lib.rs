//! Savr desktop GUI (Tauri v2).
//!
//! The GUI is a thin client: it never talks to the server directly. It sends
//! [`savr_core::ipc::GuiRequest`] frames to the always-on daemon over a local
//! socket/pipe (see [`ipc_client`]) and renders the replies. This module wires
//! up the plugins (dialog, updater, process) and registers the command handlers.

mod commands;
mod error;
mod ipc_client;

/// Build and run the Tauri application. Called by both the desktop binary
/// (`main.rs`) and the mobile entrypoint below.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Native dialogs (used by the updater confirm + error surfaces).
        .plugin(tauri_plugin_dialog::init())
        // In-app updates (PRD-04 / PRD-07 §5); config lives in tauri.conf.json.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Lets the frontend relaunch the app after an update installs.
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            commands::list_games,
            commands::list_roots,
            commands::add_root,
            commands::remove_root,
            commands::backup_now,
            commands::list_versions,
            commands::restore,
            commands::resolve_conflict,
            commands::get_status,
            commands::get_config,
            commands::update_config,
            commands::enter_learn_mode,
            commands::pair_device,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Savr application");
}
