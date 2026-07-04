//! WebSocket client (PRD-04 §4). Maintains a persistent per-device channel to
//! the server: sends `Hello` (+ `Subscribe`) on connect, handles
//! `version_available` / `conflict` / `config_updated` pushes, heartbeats with
//! `ping`, and reconnects with exponential backoff.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use savr_core::protocol::{ClientMsg, ServerMsg};

use crate::backup::backoff_delay;
use crate::engine::Engine;

const HEARTBEAT: Duration = Duration::from_secs(30);

/// Run the WebSocket client until `shutdown` flips. Reconnects on drop.
pub async fn run_ws_client(engine: Arc<Engine>, mut shutdown: watch::Receiver<bool>) {
    let mut attempt: u32 = 0;
    loop {
        if *shutdown.borrow() {
            return;
        }
        match connect_and_run(&engine, &mut shutdown).await {
            Ok(()) => attempt = 0, // clean close (usually shutdown)
            Err(e) => {
                attempt = attempt.saturating_add(1);
                tracing::debug!("ws disconnected: {e}");
            }
        }
        engine
            .server_connected_flag()
            .store(false, Ordering::Relaxed);
        if *shutdown.borrow() {
            return;
        }
        let delay = backoff_delay(attempt);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.changed() => {}
        }
    }
}

async fn connect_and_run(
    engine: &Arc<Engine>,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let client = engine.client();
    let token = client
        .access_token()
        .await
        .ok_or_else(|| anyhow::anyhow!("no access token yet; not paired"))?;
    let device_id = engine
        .device_id()
        .await
        .ok_or_else(|| anyhow::anyhow!("no device id yet; not paired"))?;

    let url = ws_url(&engine.config.server_url);
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );

    let (ws, _resp) = connect_async(request).await?;
    let (mut write, mut read) = ws.split();
    engine
        .server_connected_flag()
        .store(true, Ordering::Relaxed);
    tracing::info!("ws connected to {url}");

    // Identify + subscribe to everything (PRD-04 §4).
    let hello = ClientMsg::Hello {
        device_id,
        last_seq: HashMap::new(),
    };
    write.send(text(&hello)?).await?;
    write
        .send(text(&ClientMsg::Subscribe {
            games: vec!["*".to_string()],
        })?)
        .await?;

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    let _ = write.send(Message::Close(None)).await;
                    return Ok(());
                }
            }
            _ = heartbeat.tick() => {
                write.send(text(&ClientMsg::Ping)?).await?;
            }
            frame = read.next() => {
                let Some(frame) = frame else { break };
                match frame? {
                    Message::Text(t) => {
                        match serde_json::from_str::<ServerMsg>(t.as_ref()) {
                            Ok(msg) => handle(engine, msg).await,
                            Err(e) => tracing::debug!("unparseable server msg: {e}"),
                        }
                    }
                    Message::Ping(p) => write.send(Message::Pong(p)).await?,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn handle(engine: &Arc<Engine>, msg: ServerMsg) {
    match msg {
        ServerMsg::VersionAvailable {
            game_id,
            version_id,
            ..
        } => engine.on_version_available(game_id, version_id).await,
        ServerMsg::Conflict { game_id, tips } => engine.record_ws_conflict(game_id, tips).await,
        ServerMsg::ConfigUpdated { .. } => {
            if let Err(e) = engine.pull_config().await {
                tracing::warn!("config pull failed: {e}");
            }
        }
        ServerMsg::DeviceAdded { .. } | ServerMsg::Pong => {}
    }
}

fn text<T: serde::Serialize>(msg: &T) -> anyhow::Result<Message> {
    Ok(Message::text(serde_json::to_string(msg)?))
}

/// Map the REST base URL to the WebSocket endpoint (`https`→`wss`, `+ /ws`).
fn ws_url(server_url: &str) -> String {
    let base = server_url.trim_end_matches('/');
    let swapped = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{swapped}/ws")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_scheme_swap() {
        assert_eq!(ws_url("https://nas.local:8080"), "wss://nas.local:8080/ws");
        assert_eq!(ws_url("http://localhost:8080/"), "ws://localhost:8080/ws");
    }
}
