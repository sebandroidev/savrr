//! WebSocket per-device channel (PRD-04 §4). Auth is enforced on the HTTP
//! upgrade (the `Authed` extractor on the route), so by the time we own a socket
//! the account+device are known and the device is not revoked.
//!
//! On `Hello` we register the socket in the hub keyed by the *token's* device id
//! (not the id in the message body — that would let a client hijack another
//! device's channel) and replay any `version_available` the device missed while
//! offline, per PRD-03 §5. A 30 s ping keeps the connection warm and detects
//! dead peers.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use savr_core::protocol::{ClientMsg, ServerMsg};

use crate::api::{AppState, Authed};
use crate::db;

const HEARTBEAT: Duration = Duration::from_secs(30);

pub async fn handle_socket(socket: WebSocket, app: AppState, auth: Authed) {
    let account = auth.account_id;
    let device = auth.device_id;

    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();

    // Register up front so a push that races the client's Hello is not lost.
    app.hub.register(account, device, tx.clone());

    let mut hb = tokio::time::interval(HEARTBEAT);
    hb.tick().await; // discard the immediate first tick

    loop {
        tokio::select! {
            // Hub -> socket.
            maybe = rx.recv() => match maybe {
                Some(msg) => {
                    let json = serde_json::to_string(&msg).unwrap_or_default();
                    if sink.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                None => break, // all hub senders dropped
            },

            // Heartbeat: re-check revocation, then ping. We cannot rely on the
            // hub sender dropping to tear this socket down — the loop holds its
            // own `tx` clone — so a device revoked mid-session is closed here,
            // within one heartbeat (PRD-06 §3 "drops its WS"). The access JWT's
            // exp is likewise only checked on upgrade, so this bounds a stale
            // token's live channel to one heartbeat too.
            _ = hb.tick() => {
                if !matches!(db::device_active(&app.pool, account, device).await, Ok(true)) {
                    break;
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }

            // Socket -> handler.
            item = stream.next() => match item {
                Some(Ok(Message::Text(txt))) => {
                    match serde_json::from_str::<ClientMsg>(&txt) {
                        Ok(ClientMsg::Hello { device_id: _, last_seq }) => {
                            // A revoked device must not re-register itself over a
                            // still-open socket, so re-check before re-affirming.
                            if !matches!(db::device_active(&app.pool, account, device).await, Ok(true)) {
                                break;
                            }
                            // Re-affirm registration under the authenticated
                            // device id, then replay what was missed offline.
                            app.hub.register(account, device, tx.clone());
                            if let Ok(missed) = db::versions_since(&app.pool, account, &last_seq).await {
                                for (game_id, version_id, seq) in missed {
                                    let _ = tx.send(ServerMsg::VersionAvailable { game_id, version_id, seq });
                                }
                            }
                            let _ = db::touch_device(&app.pool, account, device).await;
                        }
                        // Single account => the socket already sees every game;
                        // Subscribe is accepted as a no-op for protocol parity.
                        Ok(ClientMsg::Subscribe { .. }) => {}
                        Ok(ClientMsg::Ping) => {
                            let _ = tx.send(ServerMsg::Pong);
                        }
                        Err(_) => {} // ignore malformed frames
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if sink.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // Binary: unused
                Some(Err(_)) => break,
            },
        }
    }

    app.hub.deregister(account, device);
}
