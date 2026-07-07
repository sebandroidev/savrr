//! Savr desktop GUI (Tauri v2).
//!
//! The GUI is a thin client: it never talks to the server directly. It sends
//! [`savr_core::ipc::GuiRequest`] frames to the daemon over a local socket/pipe
//! (see [`ipc_client`]) and renders the replies. The daemon ships bundled
//! inside this app (a Tauri sidecar) and is started, supervised, and stopped by
//! [`daemon`]; the app minimises to the tray so it keeps syncing while the
//! window is closed.

mod commands;
mod daemon;
mod error;
mod ipc_client;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, RunEvent, WindowEvent,
};

/// Show and focus the main window (used by the tray "Open" item / left-click).
fn show_main(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// Build and run the Tauri application. Called by both the desktop binary
/// (`main.rs`) and the mobile entrypoint below.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    // Single-instance MUST be registered before other plugins: a second launch
    // (clicking the app/tray again while it's already running) forwards to the
    // live instance and shows its window instead of starting a second app —
    // which would spawn a second daemon fighting over the socket + database.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        show_main(app);
    }));

    builder
        // Spawns/supervises the bundled daemon sidecar.
        .plugin(tauri_plugin_shell::init())
        // Native dialogs (used by the updater confirm + error surfaces).
        .plugin(tauri_plugin_dialog::init())
        // In-app updates (PRD-04 / PRD-07 §5); config lives in tauri.conf.json.
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Lets the frontend relaunch the app after an update installs.
        .plugin(tauri_plugin_process::init())
        // Native OS toasts (game start/close + backup outcomes).
        .plugin(tauri_plugin_notification::init())
        .manage(daemon::Supervisor::default())
        .setup(|app| {
            let handle = app.handle();

            // Start the bundled daemon (no-op in dev if it isn't staged).
            daemon::start(handle);

            // Subscribe to the daemon's pushed events and turn game start/close
            // + backup outcomes into OS toasts (runs even while hidden to tray).
            tauri::async_runtime::spawn(ipc_client::run_event_subscription(handle.clone()));

            // System tray: the app lives here while the window is hidden, so
            // the daemon keeps running in the background (PRD-07 §3).
            let open = MenuItem::with_id(app, "open", "Open Savr", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit Savr", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Savr")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "quit" => {
                        app.state::<daemon::Supervisor>().shutdown();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // Closing the window hides it to the tray instead of quitting, so a
            // background sync keeps happening. "Quit Savr" is the real exit.
            if let Some(win) = app.get_webview_window("main") {
                let w = win.clone();
                win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let _ = w.hide();
                        api.prevent_close();
                    }
                });
            }

            Ok(())
        })
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
            commands::set_autostart,
            commands::restart_for_update,
            commands::get_config,
            commands::update_config,
            commands::enter_learn_mode,
            commands::pair_device,
        ])
        .build(tauri::generate_context!())
        .expect("error while running the Savr application")
        .run(|app, event| match event {
            // macOS: clicking the Dock icon while the window is hidden to the
            // tray should bring it back (the tray isn't the only way in).
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => show_main(app),
            // Last-ditch: whatever ends the app, don't leave the daemon orphaned.
            RunEvent::Exit => app.state::<daemon::Supervisor>().shutdown(),
            _ => {}
        });
}
