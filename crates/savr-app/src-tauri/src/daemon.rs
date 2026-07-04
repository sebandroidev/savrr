//! Bundled-daemon supervisor.
//!
//! The daemon ships *inside* the app bundle as a Tauri sidecar (see
//! `tauri.conf.json` → `bundle.externalBin`) so a single install gives you the
//! whole thing. On launch the app spawns it, keeps it alive if it crashes, and
//! kills it when the app quits. The GUI still talks to it only over the local
//! IPC socket/pipe — spawning it here changes *who starts the daemon*, not how
//! they communicate.
//!
//! Two things it deliberately does NOT do: launch a second daemon when one is
//! already running (a separately-installed service, or a leftover from a hard
//! app kill), and leave the daemon orphaned if a quit races the spawn. See
//! [`Supervisor::adopt`] and the probe at the top of [`supervise`].

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

/// Name of the sidecar as declared in `bundle.externalBin`. Tauri appends the
/// target triple (and `.exe` on Windows) to find the actual file.
const SIDECAR: &str = "savr-daemon";

/// How long to wait before restarting a daemon that exited on its own.
const RESTART_DELAY: Duration = Duration::from_secs(2);

/// Give up respawning after this many restarts that each died almost instantly —
/// that's a crash loop, not a transient blip, and hammering it helps no one.
// ponytail: fixed threshold, no fancy backoff. Upgrade path: exponential
// backoff + a tray "daemon unavailable" state if crash loops show up in the wild.
const MAX_FAST_RESTARTS: u32 = 5;
const FAST_RESTART: Duration = Duration::from_secs(3);

/// The child handle and the shutdown flag live under ONE lock so the hand-off in
/// [`Supervisor::adopt`] and [`Supervisor::shutdown`] can't interleave and leak
/// a live process.
#[derive(Default)]
struct Inner {
    child: Option<CommandChild>,
    shutting_down: bool,
}

/// Shared handle to the running daemon so we can stop it on quit. Lives in Tauri
/// state via `app.manage(...)`.
#[derive(Default)]
pub struct Supervisor {
    inner: Mutex<Inner>,
}

impl Supervisor {
    /// Hand a freshly-spawned daemon to the supervisor. Returns `false` (after
    /// killing the child) if a shutdown already happened. This is the atomic
    /// hand-off: because it takes the same lock as [`shutdown`], a quit that
    /// races the spawn either kills the child here or is seen by `shutdown` —
    /// never both-miss, so the daemon is never orphaned.
    fn adopt(&self, child: CommandChild) -> bool {
        let mut g = self.inner.lock().unwrap();
        if g.shutting_down {
            let _ = child.kill();
            false
        } else {
            g.child = Some(child);
            true
        }
    }

    fn clear_child(&self) {
        self.inner.lock().unwrap().child = None;
    }

    fn is_shutting_down(&self) -> bool {
        self.inner.lock().unwrap().shutting_down
    }

    /// Stop supervising and kill the daemon. Safe to call more than once.
    pub fn shutdown(&self) {
        let mut g = self.inner.lock().unwrap();
        g.shutting_down = true;
        if let Some(child) = g.child.take() {
            let _ = child.kill();
        }
    }
}

/// Try to start the bundled daemon and, if it's there, keep it alive for the
/// life of the app. Returns immediately; supervision runs on a background task.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        supervise(app).await;
    });
}

async fn supervise(app: AppHandle) {
    // If a daemon is already listening (a separately-run standalone daemon, or
    // one left behind by a hard-killed previous session), use it instead of
    // launching a second one that would fight over the same socket + database.
    if crate::ipc_client::is_daemon_running().await {
        tracing::info!("a daemon is already running; using it instead of the bundled one");
        return;
    }

    let sup = app.state::<Supervisor>();
    let mut fast_restarts = 0u32;

    loop {
        if sup.is_shutting_down() {
            return;
        }

        let (mut rx, child) = match app.shell().sidecar(SIDECAR).and_then(|cmd| cmd.spawn()) {
            Ok(pair) => pair,
            Err(e) => {
                // A genuinely absent binary (typical in `tauri dev` — nobody
                // staged the sidecar) surfaces as io NotFound: stand down and
                // let a separately-run daemon serve the GUI. Any OTHER spawn
                // error (fd/thread/memory exhaustion under game load, a briefly
                // unreadable file) is transient — treat it like a crash and go
                // through the restart budget instead of quitting for the session.
                if is_missing_binary(&e) {
                    tracing::warn!(
                        "no bundled daemon to launch ({e}); expecting an externally-run daemon"
                    );
                    return;
                }
                tracing::error!("failed to spawn bundled daemon ({e}); will retry");
                fast_restarts += 1;
                if fast_restarts >= MAX_FAST_RESTARTS {
                    tracing::error!(
                        "bundled daemon failed to spawn {MAX_FAST_RESTARTS}×; giving up"
                    );
                    return;
                }
                tokio::time::sleep(RESTART_DELAY).await;
                continue;
            }
        };

        // Atomic hand-off: if a shutdown slipped in during spawn, adopt() kills
        // the child and we stop, so the daemon is never left running orphaned.
        if !sup.adopt(child) {
            return;
        }
        tracing::info!("bundled daemon started");

        let started = tokio::time::Instant::now();
        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(bytes) | CommandEvent::Stderr(bytes) => {
                    let line = String::from_utf8_lossy(&bytes);
                    let line = line.trim_end();
                    if !line.is_empty() {
                        tracing::info!(target: "savr_daemon", "{line}");
                    }
                }
                CommandEvent::Terminated(payload) => {
                    tracing::warn!("bundled daemon exited (code {:?})", payload.code);
                    break;
                }
                CommandEvent::Error(e) => tracing::error!("daemon pipe error: {e}"),
                _ => {}
            }
        }
        sup.clear_child();

        if sup.is_shutting_down() {
            return;
        }

        // Crash-loop guard: if it keeps dying within a few seconds, stop trying.
        if started.elapsed() < FAST_RESTART {
            fast_restarts += 1;
            if fast_restarts >= MAX_FAST_RESTARTS {
                tracing::error!(
                    "bundled daemon crash-looped {MAX_FAST_RESTARTS}× on startup; giving up"
                );
                return;
            }
        } else {
            fast_restarts = 0;
        }

        tracing::warn!("restarting bundled daemon in {}s", RESTART_DELAY.as_secs());
        tokio::time::sleep(RESTART_DELAY).await;
    }
}

/// Whether a spawn error means the sidecar binary simply isn't staged (dev),
/// as opposed to a transient OS failure worth retrying.
fn is_missing_binary(e: &tauri_plugin_shell::Error) -> bool {
    matches!(e, tauri_plugin_shell::Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound)
}
