# PRD-00 · Product Overview

> **Working name:** *Savr* (placeholder – run a proper naming pass later).
> **One line:** Self-hosted, cross-platform "Steam Cloud for every game" – auto-detects when you start/stop a game, backs the saves up to your own server, and syncs them to your other machines.

---

## 1. Problem

Steam Cloud only covers Steam games that opt in. Everything else – GOG, Epic, itch, emulators, cracked-offline, DRM-free, launcher-less – has no unified save sync. Players who game across a desktop, a laptop, and a Steam Deck either lose progress, copy folders by hand, or juggle Syncthing rules. There is no single tool that is cross-platform **and** cross-store **and** self-hosted.

## 2. Solution

A three-part system owned entirely by the user:

1. **`savr-daemon`** – a tiny, always-on, headless Rust service on each machine. Watches processes, detects game start/stop, backs up saves, syncs.
2. **`savr-app`** – a Tauri v2 GUI, opened on demand, to add games, add storage roots, resolve conflicts, and browse/restore history.
3. **`savr-server`** – an Axum service in Docker on the user's NAS/home server. The source of truth: versioned, deduplicated save history + real-time push to other devices.

## 3. Goals

- **G1** – Zero-touch backup: after setup, saves back up automatically on game exit, no clicks.
- **G2** – Cross-platform: Windows, Linux, macOS from one codebase.
- **G3** – Cross-store: Steam, GOG, Epic, emulators, manual – anything with a save folder.
- **G4** – Self-hosted: user's data never leaves hardware they control.
- **G5** – Tiny idle footprint: daemon < 30 MB RAM idle, near-zero CPU when no game runs.
- **G6** – Never lose progress: immutable versioned history + safe conflict handling.

## 4. Non-goals (v1)

- Not a game launcher or library manager (integrations only, see PRD-02).
- No public multi-tenant SaaS. Single owner, multiple trusted devices.
- No mobile client in v1 (server API stays mobile-ready for later).
- No save *editing* / cheat features.
- No peer-to-peer (LAN) sync in v1 – all sync goes through the server.

## 5. Personas

- **The owner (primary = you).** Technical, runs a NAS, games on 2–4 machines across OSes. Wants set-and-forget.
- **The household member (secondary).** Non-technical, uses the GUI only, shares the same server account or a sub-account.

## 6. Success metrics

| Metric | Target |
|---|---|
| Idle daemon RAM | < 30 MB |
| Idle daemon CPU | < 0.1% avg |
| Backup latency after game exit | < 10 s for a 50 MB save |
| Detection accuracy (known games) | > 95% start/stop events caught |
| Cross-device notify → available | < 5 s on same LAN |
| Restore success rate | 100% (immutable versions) |

## 7. Prior art (study / reuse, do not reinvent)

- **Ludusavi** (Rust, MIT) – the closest existing tool. Cross-platform, cross-store save backup. We reuse its **manifest** (see below) and borrow its differential-backup + duplicate/conflict UX. What it lacks and we add: an always-on daemon, automatic start/stop detection without a launcher, a self-hosted server, and cross-device push sync.
- **Ludusavi Manifest** – community save-location database sourced from PCGamingWiki + Steam API, designed to be reused by any tool. This is our games DB (PRD-02).
- **Steam Cloud** – UX north star for "it just syncs".
- **Syncthing** – mental model for the device ↔ device "notify + pull latest" layer (but we are hub-and-spoke via the server, not P2P).

## 8. Glossary

- **Root** – a folder the user registers where games/saves may live (a drive, a Steam library, an emulator data dir).
- **Manifest** – the games DB entry describing where a game stores saves (path templates + placeholders + registry keys).
- **Snapshot** – the current on-disk state of one game's saves, as a file→hash map.
- **Version** – an immutable, uploaded backup of a snapshot (archive blob + metadata). The unit of history.
- **Head** – the server's current pointer to the latest version of a game for an account.
- **Conflict** – two devices advanced a game's head from the same parent independently.

## 9. Suite index

| File | Contents |
|---|---|
| PRD-00 | This overview |
| PRD-01 | System architecture, tech stack, repo layout |
| PRD-02 | Detection engine (games DB, roots, process watching, per-OS) |
| PRD-03 | Sync engine (hashing, versioning, conflict, restore) |
| PRD-04 | Server API (Axum REST + WebSocket) |
| PRD-05 | Data models (Rust core types, DB schema, IPC) |
| PRD-06 | Security & auth |
| PRD-07 | Deployment (Docker on NAS, service install) |
| PRD-08 | Roadmap, MVP scope, open questions |
