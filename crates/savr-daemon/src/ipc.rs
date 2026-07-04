//! Daemon↔GUI IPC server (PRD-05 §4). Listens on a well-known local socket
//! (unix socket / Windows named pipe via `interprocess`), reads length-prefixed
//! `GuiRequest` frames, dispatches them to the [`Engine`], and writes back
//! `DaemonMsg` frames. Each connection also gets a live stream of
//! `DaemonMsg::Event` detection events.
//!
//! The per-connection request/response logic lives in [`serve_connection`],
//! generic over any async stream, so it can be driven by an in-memory duplex in
//! tests without a real socket.

use std::sync::Arc;

use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::{ListenerOptions, Name};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch};

use savr_core::ipc::{encode_frame, read_frame, DaemonMsg, GuiRequest};

use crate::engine::Engine;

#[cfg(not(windows))]
use interprocess::local_socket::GenericFilePath;
#[cfg(windows)]
use interprocess::local_socket::GenericNamespaced;

/// How many outbound frames may queue per connection before backpressure.
const OUT_BUFFER: usize = 64;

/// Run the IPC listener until `shutdown` flips to `true`.
pub async fn run_ipc_server(
    engine: Arc<Engine>,
    path: String,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // The endpoint may live under $XDG_RUNTIME_DIR/savr/ — create the parent
    // dir, and clear any leftover socket file from a crash (both unix-only).
    #[cfg(unix)]
    {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _ = std::fs::remove_file(&path);
    }

    let name = socket_name(&path)?;
    let listener = ListenerOptions::new().name(name).create_tokio()?;
    tracing::info!("ipc listening on {path}");

    loop {
        tokio::select! {
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            conn = listener.accept() => {
                match conn {
                    Ok(stream) => {
                        let engine = engine.clone();
                        tokio::spawn(async move {
                            if let Err(e) = serve_connection(engine, stream).await {
                                tracing::debug!("ipc connection ended: {e}");
                            }
                        });
                    }
                    Err(e) => tracing::warn!("ipc accept error: {e}"),
                }
            }
        }
    }

    #[cfg(unix)]
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[cfg(windows)]
fn socket_name(path: &str) -> std::io::Result<Name<'_>> {
    path.to_ns_name::<GenericNamespaced>()
}

#[cfg(not(windows))]
fn socket_name(path: &str) -> std::io::Result<Name<'_>> {
    path.to_fs_name::<GenericFilePath>()
}

/// Serve one client connection: dispatch requests and stream events until the
/// client disconnects. Generic over the transport so tests can use
/// `tokio::io::duplex`.
pub async fn serve_connection<S>(engine: Arc<Engine>, stream: S) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Single writer task drains an mpsc so request replies and pushed events
    // never interleave mid-frame.
    let (out_tx, mut out_rx) = mpsc::channel::<DaemonMsg>(OUT_BUFFER);
    let writer_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let frame = match encode_frame(&msg) {
                Ok(f) => f,
                Err(e) => {
                    // Defensive: DaemonMsg is expected to always encode (the
                    // Vec-newtype variants serialize fine now that DaemonMsg is
                    // adjacently tagged in savr-core). If some future variant
                    // ever fails, degrade to an Error frame rather than dropping
                    // the connection so the GUI still gets a diagnostic.
                    tracing::error!("cannot encode reply: {e}");
                    match encode_frame(&DaemonMsg::Error {
                        message: format!("daemon could not encode reply: {e}"),
                    }) {
                        Ok(f) => f,
                        Err(_) => break,
                    }
                }
            };
            if writer.write_all(&frame).await.is_err() || writer.flush().await.is_err() {
                break;
            }
        }
    });

    // Forward detection events to this client as they happen (PRD-05 §4).
    let event_tx = out_tx.clone();
    let mut events = engine.subscribe();
    let forward_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if event_tx.send(DaemonMsg::Event(event)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("ipc client lagged, dropped {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Request/response loop.
    loop {
        let req: Option<GuiRequest> = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            Err(e) => {
                tracing::debug!("ipc frame read error: {e}");
                break;
            }
        };
        let Some(req) = req else {
            break; // clean EOF at a frame boundary
        };
        let reply = engine.handle_request(req).await;
        if out_tx.send(reply).await.is_err() {
            break;
        }
    }

    forward_task.abort();
    drop(out_tx); // closing all senders lets the writer task finish
    let _ = writer_task.await;
    Ok(())
}
