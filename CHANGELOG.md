# Changelog

Notable changes to Savrr. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions aim for [semantic versioning](https://semver.org/spec/v2.0.0.html) once past 1.0.

## [0.1.8] - 2026-07-07

Turning on "Start on Windows sign-in" works.

### Fixed

- Enabling "Start on Windows sign-in" failed with a serialization error and never took effect. The setting now applies correctly.

## [0.1.7] - 2026-07-07

Steam Cloud saves get backed up, and Savr can run at sign-in.

### Added

- Savr now backs up Steam Cloud saves — the ones Steam keeps under `userdata`. Games like Dead Cells that store their saves there (and thousands of others) are now captured instead of being silently skipped.
- A new setting, "Start on Windows sign-in," runs Savr in the background at login with no window. Your games are detected and saved even when you're in Xbox Full Screen mode and never open the app.

### Fixed

- The background daemon now keeps its own log file, so problems can be diagnosed even when it runs headless with no console.

## [0.1.6] - 2026-07-06

See at a glance whether Savr is watching your games.

### Added

- The Games tab now shows a live "Playing" badge the moment Savr detects a game start, plus when each game was last played and how long your last and total sessions ran. This works even for games Savr doesn't have a save location for yet, so it's a direct way to confirm detection is working.

### Fixed

- A backup running after you close one game no longer delays Savr noticing the next game you launch — detection and the "Playing" badge stay responsive while a save uploads in the background.

## [0.1.5] - 2026-07-06

Version history works again, and Savr shows its own version.

### Fixed

- Version history and backups failed after pairing with a "localhost:8080" error. Pairing stored where your server lives, but every restart the daemon went back to its built-in default and ignored it. It now uses your paired server on startup, so version history and backups reach the right place.

### Added

- Savr's own version is shown at the bottom of the sidebar, so you can tell at a glance which build you're on.

## [0.1.4] - 2026-07-06

Your games show up right away.

### Fixed

- The games list could come up empty right after launching or updating Savr. The app was asking for your games while the daemon was still starting up, then never asked again. Savr now builds the list before that slow step and reloads on its own the moment it's ready, so your games appear within a second instead of not at all.

## [0.1.3] - 2026-07-06

Savr tells you when it's working.

### Added

- Desktop notifications around your games, in the spirit of Steam's cloud-save toasts. When a game starts, Savr says it's watching; when you close it, Savr confirms your save was backed up. You'll also get a heads-up when a save from another device is waiting, or when two saves conflict. On Windows the toast shows the real Savr name and icon once the app is installed.

### Fixed

- Two Savr icons showed up in the Windows system tray instead of one.

## [0.1.2] - 2026-07-04

Your games stick around, and Savr sees more of them.

### Fixed

- After pairing, relaunching the app showed an empty games list: a server call that failed during startup threw away the whole catalog. The catalog now survives a server hiccup and falls back to a local id until the server is reachable again.
- "Back up now" on a game whose save location Savr doesn't know yet no longer claims it queued a backup. It tells you to turn on Learn mode and play the game once so Savr can find the saves.
- Restoring a game with no known save paths is refused instead of writing and deleting files under your home directory.

### Changed

- Savr lists every installed Steam game, not just the ones already in its save-location database. Ones it doesn't recognize yet still appear; play one with Learn mode on and it learns where that game saves. Steam's own plumbing — Proton, the Linux runtimes, the redistributables bundle, SteamVR — is left out, and titles that are still downloading are held back until they finish installing.
- A Steam game is matched across your devices by its Steam app id, so two devices that show it under slightly different names no longer split its history into two.

## [0.1.1] - 2026-07-04

The daemon moves inside the app.

### Changed

- The desktop app now bundles the daemon and runs it for you, so installing the app is the whole install. It starts the daemon on launch, restarts it if it crashes, and stops it when you quit. Closing the window hides the app to the system tray instead of quitting, so a background sync keeps going; "Quit Savr" from the tray is the real exit. If a daemon is already running — the headless binary is still published for servers — the app uses it instead of starting a second one, and a second app launch just reopens the running window rather than spawning another.

## [0.1.0] - 2026-07-04

The first working slice of the whole system: shared core, server, headless daemon, and desktop app.

### Added

- **savr-core**: shared types for the whole suite; a Ludusavi manifest parser with placeholder resolution; blake3 snapshotting and diffing; the `.savr` archive format (zstd + tar) with path-traversal-safe extraction; and the REST, WebSocket, and daemon/GUI IPC contracts.
- **savr-server**: an Axum service backed by SQLite and a content-addressed blob store. Immutable versioned history, deduplicated blobs, and a compare-and-swap that advances a game's head or records a conflicting branch instead of overwriting it. Device pairing, JWT access tokens with refresh, per-device revocation, a WebSocket push channel that replays what a device missed while offline, conflict resolution, synced config, and retention-based garbage collection.
- **savr-daemon**: a headless service that watches running processes, matches them to games, and backs up saves when a game exits. Manifest sync with ETag caching, Steam library parsing, a diff-and-upload pipeline with an offline outbox, safe restore, and a local socket for the GUI to talk to.
- **savr-app**: a Tauri v2 desktop app with a Svelte UI for pairing, managing watched folders, browsing history, restoring saves, and resolving conflicts. Updates itself through GitHub releases.
- Docker image and Compose file for the server, and CI plus release automation for all four pieces.

### Security

- An adversarial code review before release fixed a device-revocation gap (a revoked device kept its WebSocket), an offline-differential backup inconsistency, a config-sync tag drop, and a device-state oracle on `/auth/refresh`. Remaining known issues are tracked in [docs/KNOWN-ISSUES.md](docs/KNOWN-ISSUES.md).

[0.1.8]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.8
[0.1.7]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.7
[0.1.6]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.6
[0.1.5]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.5
[0.1.4]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.4
[0.1.3]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.3
[0.1.2]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.2
[0.1.1]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.1
[0.1.0]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.0
