# PRD-08 · Roadmap, MVP & Open Questions

---

## 1. MVP definition (the smallest thing that delivers G1–G6)

**In:**
- `savr-server` (Axum, SQLite, filesystem blobs) with REST + WebSocket.
- `savr-daemon` on **Linux + Windows** first (macOS right after): process-watch detection, manifest-based path resolution, manual override, backup on game-stop, upload, pull+restore on push.
- `savr-app` (Tauri v2): pair device, add roots, list games, manual backup, version history, restore, conflict resolve view.
- Ludusavi manifest import + ETag update.
- Full + differential backups, retention, `manual` conflict policy, `ask` auto-pull.
- Device pairing + JWT + TLS-via-WireGuard/Tailscale documented.

**Out (fast-follow):**
- macOS is the third target (days behind, same codebase).
- Client-side encryption.
- Launcher integrations (Heroic/Lutris), mDNS discovery, launch-through mode.
- Household sub-accounts, mobile client.

**Hard cut line if timeline slips:** ship the single-Tauri-app variant (tray-only, no separate daemon) first, accept higher idle RAM, split later. Architecture in PRD-01 §2 keeps this reversible.

## 2. Milestones

| # | Milestone | Deliverable | Proves |
|---|---|---|---|
| M0 | Workspace + `savr-core` | types, manifest parse + placeholder resolve, blake3 snapshot/diff | foundation compiles across tiers |
| M1 | Server skeleton | Axum + SQLite + FsBlobStore, `/versions`, `/blobs`, CAS head advance | backup can land server-side |
| M2 | Daemon backup path | detect stop → snapshot → diff → archive → upload | end-to-end backup on one machine |
| M3 | Push + restore | WebSocket, `version_available`, pull + safe restore | two machines stay in sync |
| M4 | Conflict handling | divergent branch + resolve API + GUI resolve view | never lose progress (G6) |
| M5 | GUI complete | Tauri app: pairing wizard, roots, history, status dashboard | usable by a human |
| M6 | Detection hardening | exe-index, learn-mode, per-OS save paths, Steam root parsing | > 95% detection (G5/G3) |
| M7 | Packaging | Docker image + compose; per-OS service install + bundles | installable by the owner |
| M8 | macOS + polish | launchd, mac save paths, retention/GC, footprint tuning | cross-platform (G2), tiny (G5) |

## 3. Key risks & mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Detection misses game start/stop | saves not backed up | debounce + settle time; learn-mode; optional launch-through; `notify` file-watch as backup trigger |
| Game writes save asynchronously after exit | backup captures stale save | `settle_ms` delay before reading; optional re-check |
| Manifest missing a game | no auto paths | manual override always available; contribute back to PCGamingWiki |
| Two devices edit offline | conflict | Git-lite branching + resolve UX; nothing overwritten |
| Idle RAM creeps up | breaks G5 | headless daemon (no webview), adaptive polling, `MemoryMax` guard, status dashboard to monitor |
| Large saves (100s of MB) | slow uploads | differential backups, blob dedup, resumable PUT, zstd level tuning |
| Registry-based saves (Windows) | missed on restore | capture HKCU keys into archive; restore writes them back |

## 4. Performance discipline (since this is "performance-first")

- Daemon: no webview, adaptive poll (slow when idle), `rayon` for parallel hashing, stream archives to disk (no full-buffer), `MemoryMax=64M`.
- Server: single-transaction hot path, streamed blobs, prepared statements, content-addressed dedup avoids redundant storage + transfer.
- Wire: only changed files upload (diff); dedup skips already-present blobs entirely (`HEAD` before `PUT`).

## 5. Open questions (need your call)

1. **DB default:** SQLite (my recommendation, one less container on the NAS) vs Postgres from day one? You use Postgres heavily elsewhere – happy to default Postgres if you'd rather keep one stack.
2. **TLS story you'll actually use:** Tailscale/WireGuard (simplest), reverse-proxy + domain, or self-signed pinning? This drives the pairing/first-run UX.
3. **Encryption at rest:** ship opt-in client-side encryption in v1, or defer? (Saves are rarely sensitive, but ransomware-resilience is a selling point.)
4. **Mid-session backups:** back up only on game exit (default, protects idle footprint) or also on save-file change during long sessions (via `notify`)? Opt-in per game?
5. **Frontend stack for the Tauri GUI:** React, Svelte, or Solid? (Affects your velocity, not the architecture.) Given your brand aesthetic, worth a dedicated design doc (DESIGN-00) later.
6. **Name:** *Savr* is a placeholder. Want a proper naming pass in your usual style (short, punchy, on-brand)? I can generate candidates with domain/handle availability notes.

## 6. Suggested next docs

- **DESIGN-00** – visual bible + GUI screens (roots manager, game list, version timeline, conflict resolver), in your Majora's-Mask-flavored aesthetic.
- **PRD-09** – Detection test plan: a matrix of real games per store/OS to validate the exe-index + manifest resolution.
