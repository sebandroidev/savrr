# Changelog

Notable changes to Savrr. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions aim for [semantic versioning](https://semver.org/spec/v2.0.0.html) once past 1.0.

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

[0.1.1]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.1
[0.1.0]: https://github.com/sebandroidev/savrr/releases/tag/v0.1.0
