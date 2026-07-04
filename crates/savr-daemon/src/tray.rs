//! System tray (PRD-07 §3).
//!
//! ponytail: this is a deliberate stub. `tray-icon` requires the platform GUI
//! event loop on the **main thread** (a `winit`/AppKit run loop), which is
//! fundamentally incompatible with a headless tokio service that must own the
//! main thread for graceful shutdown and never block on a UI loop. In the
//! shipping product the **GUI app owns the tray** (PRD-07 §3) and talks to this
//! daemon over IPC; the daemon stays truly headless.
//!
//! The `tray` cargo feature exists as the seam: enabling it pulls the crate in,
//! but a real tray must still be driven from a process that runs an event loop
//! on its main thread. The default build is tray-free so the service binary
//! runs anywhere (systemd, launchd, a container) with no display server.

/// Start the tray if this build asked for it. A no-op in the headless default.
pub fn spawn() {
    #[cfg(feature = "tray")]
    {
        // Upgrade path: hand off to a main-thread event loop owned by a GUI
        // shell. Building the icon here (off the main thread) would panic on
        // macOS, so we only log that the feature is compiled in.
        tracing::info!("tray feature compiled in; tray is driven by the GUI shell, not the daemon");
    }
    #[cfg(not(feature = "tray"))]
    {
        tracing::debug!("tray disabled (headless build)");
    }
}
