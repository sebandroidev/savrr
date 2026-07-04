//! Process-watching detection engine (PRD-02 §3): a lightweight `sysinfo`
//! poller with an adaptive interval that emits `GameStarted` / `GameStopped`.

pub mod exe_index;
pub mod steam;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use sysinfo::{ProcessesToUpdate, System};
use tokio::sync::{broadcast, watch, RwLock};

use savr_core::ipc::DetectionEvent;
use savr_core::GameId;

pub use exe_index::ExeIndex;

/// Shared, hot-swappable exe index (rebuilt when roots/games change).
pub type SharedExeIndex = Arc<RwLock<ExeIndex>>;

/// Tunables for the poll loop (PRD-02 §3.1).
#[derive(Debug, Clone, Copy)]
pub struct WatchTuning {
    pub active_interval: Duration,
    pub idle_interval: Duration,
    pub settle: Duration,
}

impl WatchTuning {
    /// Pick the next sleep: fast while a game is running or a stop is settling,
    /// slow when idle (keeps CPU ~0 — PRD-02 §3.1, G5).
    pub fn next_interval(&self, busy: bool) -> Duration {
        if busy {
            self.active_interval
        } else {
            self.idle_interval
        }
    }
}

/// Runs the poll loop until `shutdown` flips to `true`. Emits detection events
/// on `events`. Reads the exe index under a read lock each tick so a concurrent
/// rebuild is picked up without restarting the loop.
pub async fn run_watcher(
    index: SharedExeIndex,
    tuning: WatchTuning,
    events: broadcast::Sender<DetectionEvent>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut sys = System::new();
    // game_id -> pid of the process we matched, for currently-running games.
    let mut running: HashMap<GameId, u32> = HashMap::new();
    // game_id -> deadline after which an absent game is confirmed stopped.
    let mut pending_stop: HashMap<GameId, Instant> = HashMap::new();

    loop {
        let now_running = scan_running_games(&mut sys, &index).await;

        // Newly started games.
        for (&game_id, &pid) in &now_running {
            if !running.contains_key(&game_id) {
                let _ = events.send(DetectionEvent::GameStarted {
                    game_id,
                    pid,
                    at: Utc::now(),
                });
            }
            // A reappearance cancels a pending stop (debounce flaps).
            pending_stop.remove(&game_id);
        }

        // Games that vanished this tick → arm a settle timer.
        for &game_id in running.keys() {
            if !now_running.contains_key(&game_id) {
                pending_stop
                    .entry(game_id)
                    .or_insert_with(|| Instant::now() + tuning.settle);
            }
        }

        // Fire settled stops.
        let now = Instant::now();
        let fired: Vec<GameId> = pending_stop
            .iter()
            .filter(|(g, deadline)| !now_running.contains_key(*g) && now >= **deadline)
            .map(|(g, _)| *g)
            .collect();
        for game_id in fired {
            pending_stop.remove(&game_id);
            let _ = events.send(DetectionEvent::GameStopped {
                game_id,
                at: Utc::now(),
            });
        }

        running = now_running;

        let busy = !running.is_empty() || !pending_stop.is_empty();
        let sleep = tuning.next_interval(busy);

        tokio::select! {
            _ = tokio::time::sleep(sleep) => {}
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    tracing::debug!("watcher shutting down");
                    return;
                }
            }
        }
    }
}

/// One poll: refresh processes and resolve the set of running games.
async fn scan_running_games(sys: &mut System, index: &SharedExeIndex) -> HashMap<GameId, u32> {
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let idx = index.read().await;
    let mut out: HashMap<GameId, u32> = HashMap::new();
    // Dedup identical exes across many PIDs so we do one lookup per exe.
    let mut seen: HashSet<String> = HashSet::new();
    for (pid, proc_) in sys.processes() {
        let Some(exe) = proc_.exe() else { continue };
        let key = exe.to_string_lossy().into_owned();
        if !seen.insert(key) {
            continue;
        }
        if let Some((game_id, _conf)) = idx.match_exe(exe) {
            out.entry(game_id).or_insert_with(|| pid.as_u32());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_interval() {
        let t = WatchTuning {
            active_interval: Duration::from_secs(3),
            idle_interval: Duration::from_secs(12),
            settle: Duration::from_secs(5),
        };
        assert_eq!(t.next_interval(true), Duration::from_secs(3));
        assert_eq!(t.next_interval(false), Duration::from_secs(12));
    }
}
