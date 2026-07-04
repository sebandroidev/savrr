//! In-memory WebSocket presence hub (PRD-04 §4). Maps `account -> device -> the
//! channel that feeds that device's socket`. The router hands each live socket
//! an unbounded sender registered here; broadcasts fan a `ServerMsg` to the
//! relevant senders.
//!
//! ponytail: single-process, memory-resident presence. A multi-node deployment
//! would need shared pub/sub (Redis/NATS) so a push on node A reaches a socket
//! on node B — out of scope for the single-owner home server, and the broadcast
//! API is the seam to swap in later.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use uuid::Uuid;

use savr_core::protocol::ServerMsg;

type DeviceMap = HashMap<Uuid, mpsc::UnboundedSender<ServerMsg>>;

#[derive(Clone, Default)]
pub struct Hub {
    inner: Arc<Mutex<HashMap<Uuid, DeviceMap>>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a device's outbound channel. Idempotent: a
    /// reconnect or a post-Hello re-register just overwrites the sender.
    pub fn register(&self, account: Uuid, device: Uuid, tx: mpsc::UnboundedSender<ServerMsg>) {
        self.inner
            .lock()
            .unwrap()
            .entry(account)
            .or_default()
            .insert(device, tx);
    }

    pub fn deregister(&self, account: Uuid, device: Uuid) {
        let mut g = self.inner.lock().unwrap();
        if let Some(devs) = g.get_mut(&account) {
            devs.remove(&device);
            if devs.is_empty() {
                g.remove(&account);
            }
        }
    }

    /// Fan `msg` to every device on `account` except `except` — the device that
    /// caused the event (it already knows). Used for `version_available`.
    pub fn broadcast_except(&self, account: Uuid, except: Uuid, msg: ServerMsg) {
        let g = self.inner.lock().unwrap();
        if let Some(devs) = g.get(&account) {
            for (id, tx) in devs.iter() {
                if *id != except {
                    let _ = tx.send(msg.clone());
                }
            }
        }
    }

    /// Fan `msg` to every device on the account (including the actor). Used for
    /// conflict and config-updated notices.
    pub fn broadcast_account(&self, account: Uuid, msg: ServerMsg) {
        let g = self.inner.lock().unwrap();
        if let Some(devs) = g.get(&account) {
            for tx in devs.values() {
                let _ = tx.send(msg.clone());
            }
        }
    }

    /// Remove a revoked device from the presence map so it stops receiving
    /// broadcasts immediately (PRD-06 §3). This does not by itself close the
    /// device's open socket — the socket loop holds its own sender clone — so
    /// the ws handler re-checks `device_active` each heartbeat and closes there.
    pub fn drop_device(&self, account: Uuid, device: Uuid) {
        self.deregister(account, device);
    }

    /// Count of live sockets for an account — test/observability helper.
    pub fn device_count(&self, account: Uuid) -> usize {
        self.inner
            .lock()
            .unwrap()
            .get(&account)
            .map(|d| d.len())
            .unwrap_or(0)
    }
}
