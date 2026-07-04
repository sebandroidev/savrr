# PRD-07 · Deployment & Packaging

Two deployment surfaces: the **server** (Docker on a NAS/home server) and the **desktop pieces** (daemon as a system service + GUI app installer) on each machine.

---

## 1. Server (Docker)

### 1.1 Image
- Multi-stage build: Rust builder → distroless/`debian:slim` runtime. Static-ish binary, small image.
- Single binary `savr-server`. Config via env + optional `/config/savr.toml`.

```dockerfile
# docker/Dockerfile
FROM rust:1-slim AS build
WORKDIR /app
COPY . .
RUN cargo build --release -p savr-server

FROM debian:stable-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/savr-server /usr/local/bin/savr-server
VOLUME ["/data"]                # sqlite db + blob store
EXPOSE 8080
ENTRYPOINT ["savr-server"]
```

### 1.2 Compose (SQLite + filesystem blobs = zero extra services)
```yaml
# docker/docker-compose.yml
services:
  savr:
    image: savr-server:latest
    restart: unless-stopped
    ports: ["8080:8080"]
    volumes:
      - ./data:/data
    environment:
      SAVR_DB_URL: "sqlite:///data/savr.db"
      SAVR_BLOB_BACKEND: "fs"
      SAVR_BLOB_PATH: "/data/blobs"
      SAVR_BIND: "0.0.0.0:8080"
      SAVR_OWNER_PASSWORD_FILE: "/run/secrets/owner_pw"  # set at first boot
    secrets: [owner_pw]
secrets:
  owner_pw:
    file: ./secrets/owner_pw.txt
```

### 1.3 Optional profiles
- **Postgres:** add a `postgres` service, set `SAVR_DB_URL=postgres://...`. Use when you want concurrency/analytics beyond a single user.
- **MinIO:** add `minio`, set `SAVR_BLOB_BACKEND=s3` + endpoint/keys. Use when you already run object storage.
- **TLS:** front with Caddy/Traefik, or run behind Tailscale (PRD-06 §4). Compose examples for each shipped in `docker/`.

### 1.4 Server resource footprint (targets)
- Idle RAM < 60 MB (SQLite mode). Image < 80 MB. Runs comfortably on a Raspberry Pi / low-end NAS.

## 2. Desktop daemon (system service per OS)

The daemon must start at login and stay resident. Installer registers it:

| OS | Mechanism | Notes |
|---|---|---|
| Windows | Scheduled Task at logon (user scope) or Windows Service | user scope needed for HKCU registry saves + user home paths |
| Linux | `systemd --user` unit | correct `$HOME`/XDG paths; `WantedBy=default.target` |
| macOS | `launchd` LaunchAgent (`~/Library/LaunchAgents`) | per-user; `RunAtLoad` + `KeepAlive` |

Example systemd user unit:
```ini
# ~/.config/systemd/user/savr-daemon.service
[Unit]
Description=Savr save-sync daemon
After=network-online.target

[Service]
ExecStart=%h/.local/bin/savr-daemon
Restart=on-failure
# footprint guards
MemoryMax=64M

[Install]
WantedBy=default.target
```

Daemon config: `~/.config/savr/daemon.toml` (server URL, poll interval, policies) + secrets in the OS keychain (PRD-06 §3).

## 3. GUI app packaging (Tauri v2)

- `cargo tauri build` produces per-OS bundles:
  - Windows: `.msi` / NSIS `.exe`.
  - macOS: `.dmg` / `.app` (codesign + notarize for distribution).
  - Linux: `.deb`, `.AppImage`, and a Flatpak.
- **Linux tray dependency:** declare `libayatana-appindicator` (preferred) / `libappindicator3`. The `.deb` adds the dependency; the AppImage embeds it. Document that tray hover events don't fire on Linux (click + menu only).
- The installer bundles the daemon binary and registers the service (§2) as a post-install step; the GUI's first-run wizard pairs the device (PRD-06 §2) and adds the first roots.

## 4. First-run flow (GUI)

1. Point at server URL (or auto-discover via mDNS on LAN, later).
2. Pair device with code.
3. Add roots (auto-suggest Steam libraries found on disk).
4. Initial scan: match installed games against the manifest, show what will be watched.
5. Done → daemon takes over; GUI can be closed.

## 5. Updates

- **Server:** repull image / `docker compose up -d`. DB migrations run on boot (`sqlx migrate`), forward-only, idempotent.
- **Desktop:** Tauri updater plugin for the GUI; daemon self-update via signed release check (or piggyback on GUI update). Manifest DB updates itself continuously (PRD-02 §1.1) independent of app version.

## 6. Observability (minimal, local)

- Structured logs (`tracing`) to stdout (server → Docker logs) and a rotating file (daemon).
- `/healthz` (liveness) and `/readyz` (DB + blob store reachable) on the server.
- Daemon exposes status over IPC (RAM, watched games, last backup) for the GUI's dashboard – satisfies the "prove it's tiny" story for G5.
