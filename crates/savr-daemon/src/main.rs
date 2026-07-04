//! `savr-daemon` binary: a thin launcher. Loads config, opens local state,
//! builds the engine, and fans out the long-lived tasks — detection watcher,
//! event-driven backups, WebSocket client, IPC server, outbox retry, and the
//! daily manifest refresh — over one tokio runtime with graceful shutdown
//! (PRD-07 §2). All real logic lives in the library modules.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing_subscriber::EnvFilter;

use savr_daemon::config::{config_root, data_root, DaemonConfig};
use savr_daemon::detection::{run_watcher, WatchTuning};
use savr_daemon::secrets::{self, SecretStore};
use savr_daemon::{engine::Engine, ipc, ipc_path, manifest_sync, state::LocalState, tray, ws};

const OUTBOX_INTERVAL: Duration = Duration::from_secs(30);
const MANIFEST_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

fn db_path() -> PathBuf {
    std::env::var("SAVR_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_root().join("daemon.db"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = DaemonConfig::load_default()?;
    tracing::info!("savr-daemon {} starting", env!("CARGO_PKG_VERSION"));

    let state = LocalState::open(&db_path()).await?;
    let secret_store: Arc<dyn SecretStore> = Arc::from(secrets::from_env(&config_root()));
    let (events, _keepalive) = broadcast::channel(256);
    let engine = Engine::new(config.clone(), state, secret_store, events.clone()).await?;

    // FIRST TASK (PRD-02 §1.1): fetch + parse the real manifest, log the count.
    let manifest_dir = config.manifest_dir();
    match manifest_sync::refresh(&manifest_dir).await {
        Ok(outcome) => {
            tracing::info!("manifest ready: {} entries", outcome.entry_count);
            engine.set_manifest(outcome.manifest).await;
        }
        Err(e) => {
            tracing::warn!("manifest refresh failed ({e}); trying cache");
            if let Some(m) = manifest_sync::load_cached(&manifest_dir)? {
                engine.set_manifest(m).await;
            }
        }
    }

    // First-boot convenience + build the initial catalog / exe index.
    engine.ensure_default_roots().await.ok();
    if let Err(e) = engine.refresh_games().await {
        tracing::warn!("initial catalog build failed: {e}");
    }

    tray::spawn();

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let tuning = WatchTuning {
        active_interval: Duration::from_millis(config.poll_interval_ms),
        idle_interval: Duration::from_millis(config.poll_idle_interval_ms),
        settle: Duration::from_millis(config.settle_ms),
    };

    let mut tasks: Vec<JoinHandle<()>> = Vec::new();
    tasks.push(tokio::spawn(run_watcher(
        engine.exe_index(),
        tuning,
        events.clone(),
        shutdown_rx.clone(),
    )));
    tasks.push(tokio::spawn(
        engine.clone().run_event_loop(shutdown_rx.clone()),
    ));
    tasks.push(tokio::spawn(ws::run_ws_client(
        engine.clone(),
        shutdown_rx.clone(),
    )));
    {
        let engine = engine.clone();
        let path = ipc_path();
        let rx = shutdown_rx.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = ipc::run_ipc_server(engine, path, rx).await {
                tracing::error!("ipc server error: {e}");
            }
        }));
    }
    tasks.push(tokio::spawn(outbox_loop(
        engine.clone(),
        shutdown_rx.clone(),
    )));
    tasks.push(tokio::spawn(manifest_loop(
        engine.clone(),
        manifest_dir,
        shutdown_rx.clone(),
    )));

    // Block until Ctrl-C / SIGINT, then signal every task and drain.
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown requested");
    let _ = shutdown_tx.send(true);
    for task in tasks {
        let _ = task.await;
    }
    tracing::info!("savr-daemon stopped");
    Ok(())
}

/// Periodically drain the offline upload outbox (PRD-03 §8).
async fn outbox_loop(engine: Arc<Engine>, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(OUTBOX_INTERVAL);
    loop {
        tokio::select! {
            _ = tick.tick() => engine.flush_outbox().await,
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() { return; }
            }
        }
    }
}

/// Refresh the manifest daily (PRD-02 §1.1) and rebuild the catalog on change.
async fn manifest_loop(
    engine: Arc<Engine>,
    manifest_dir: PathBuf,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(MANIFEST_INTERVAL);
    tick.tick().await; // skip the immediate tick (startup already refreshed)
    loop {
        tokio::select! {
            _ = tick.tick() => {
                match manifest_sync::refresh(&manifest_dir).await {
                    Ok(o) if !o.not_modified => {
                        engine.set_manifest(o.manifest).await;
                        let _ = engine.refresh_games().await;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("daily manifest refresh failed: {e}"),
                }
            }
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() { return; }
            }
        }
    }
}
