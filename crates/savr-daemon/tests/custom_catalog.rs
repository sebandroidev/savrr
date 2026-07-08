//! refresh_games merges Steam + auto-detected + manual games into one catalog,
//! deduped by normalized title.

use std::sync::Arc;

use savr_core::ipc::{DaemonMsg, GuiRequest, RootKind};
use savr_daemon::config::DaemonConfig;
use savr_daemon::engine::Engine;
use savr_daemon::secrets::{FileStore, SecretStore};
use savr_daemon::state::{CustomGame, LocalState};
use tokio::sync::broadcast;

async fn engine_with(manifest_yaml: &str, drive_root: &std::path::Path) -> Arc<Engine> {
    let state = LocalState::open_memory().await.unwrap();
    state
        .add_root(RootKind::Drive, &drive_root.to_string_lossy())
        .await
        .unwrap();
    state
        .add_custom_game(&CustomGame {
            title: "My Cracked Game".into(),
            install_path: None,
            save_root: drive_root.join("saves").to_string_lossy().into_owned(),
            include: vec!["**/*".into()],
            exclude: vec![],
        })
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let secret_store: Arc<dyn SecretStore> =
        Arc::new(FileStore::new(dir.path().join("creds.json")));
    std::mem::forget(dir);
    let (events, _rx) = broadcast::channel(16);
    let engine = Engine::new(DaemonConfig::default(), state, secret_store, events)
        .await
        .unwrap();
    let manifest = savr_core::manifest::parse(manifest_yaml).unwrap();
    engine.set_manifest(manifest).await;
    engine
}

#[tokio::test]
async fn merges_autodetected_and_custom_games() {
    let drive = tempfile::tempdir().unwrap();
    std::fs::create_dir(drive.path().join("Hollow Knight")).unwrap();
    let manifest = "\
Hollow Knight:
  files:
    <base>/saves: {}
  installDir:
    Hollow Knight: {}
";
    let engine = engine_with(manifest, drive.path()).await;
    engine.refresh_games(false).await.unwrap();

    let games = match engine.handle_request(GuiRequest::ListGames).await {
        DaemonMsg::Games(games) => games,
        other => panic!("expected Games, got {other:?}"),
    };
    let titles: std::collections::HashSet<String> = games.iter().map(|g| g.title.clone()).collect();
    assert!(titles.contains("Hollow Knight"), "auto-detected");
    assert!(titles.contains("My Cracked Game"), "manual");
}
