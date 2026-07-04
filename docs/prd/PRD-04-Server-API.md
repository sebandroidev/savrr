# PRD-04 · Server API (Axum)

Performance-first, minimal surface. REST for state, WebSocket for push, content-addressed blob store for archives. Shares `savr-core` types, so request/response bodies are the same Rust structs the clients use.

---

## 1. Conventions

- Base: `https://<host>:<port>/api/v1`
- Auth: `Authorization: Bearer <device_jwt>` on every call except pairing/login (PRD-06).
- Bodies: JSON (`serde`) for metadata; raw bytes for blob transfer.
- Errors: `{ "error": { "code": "conflict", "message": "...", "detail": {...} } }` + proper HTTP status.
- IDs: UUIDv7 (time-ordered) for versions/devices; server assigns a monotonic `seq` per (account, game) for ordering.

## 2. REST endpoints

### Auth / devices
| Method | Path | Purpose |
|---|---|---|
| `POST` | `/auth/login` | owner login (password) → session token |
| `POST` | `/devices/pair` | pair a new device using a one-time pairing code → device JWT |
| `GET` | `/devices` | list registered devices |
| `DELETE` | `/devices/{id}` | revoke a device |

### Games & config
| Method | Path | Purpose |
|---|---|---|
| `GET` | `/games` | list games known to the account (with head + last version) |
| `POST` | `/games` | register/ensure a game (from manifest match or custom) |
| `GET` | `/config` | fetch synced user config (custom games, overrides, policies) |
| `PUT` | `/config` | update synced config (optimistic-concurrency via version tag) |

### Versions (backup)
| Method | Path | Purpose |
|---|---|---|
| `POST` | `/games/{id}/versions` | create a version; body = `Version` meta + `parent`. Returns `201` (head advanced) or `409` (conflict, includes both tips) |
| `GET` | `/games/{id}/versions` | list version history (paginated) |
| `GET` | `/games/{id}/head` | current head version id + seq |
| `POST` | `/games/{id}/resolve` | resolve a conflict: `{ winner, policy, keep_both? }` → new head |

### Blobs (archives)
| Method | Path | Purpose |
|---|---|---|
| `HEAD` | `/blobs/{hash}` | existence check (dedup: skip upload if present) |
| `PUT` | `/blobs/{hash}` | upload archive; supports `Content-Range` for resumable |
| `GET` | `/blobs/{hash}` | download archive (for restore); range requests supported |

**Upload flow:** client `HEAD /blobs/{hash}` → if `404`, `PUT` the bytes → then `POST /versions` referencing the hash. Server rejects a version whose `blob_hash` it can't find (referential integrity).

## 3. Blob store abstraction

```rust
#[async_trait]
trait BlobStore {
    async fn exists(&self, hash: &Blake3Hash) -> Result<bool>;
    async fn put(&self, hash: &Blake3Hash, bytes: ByteStream) -> Result<()>;
    async fn get(&self, hash: &Blake3Hash, range: Option<Range>) -> Result<ByteStream>;
    async fn delete(&self, hash: &Blake3Hash) -> Result<()>; // GC only
}
```
- **`FsBlobStore`** (default): `blobs/<hh>/<hash>` on a mounted volume. Content-addressed = automatic dedup + trivial integrity check.
- **`S3BlobStore`** (optional): MinIO or S3-compatible for users who already run object storage.
Selected via config; clients never know the difference.

## 4. WebSocket (`/ws`)

Persistent per-device channel for presence + push. Auth via `Bearer` on the upgrade request.

Server → client:
```jsonc
{ "type": "version_available", "game_id": "...", "version_id": "...", "seq": 42 }
{ "type": "conflict",          "game_id": "...", "tips": ["v1","v2"] }
{ "type": "config_updated",    "config_tag": "..." }
{ "type": "device_added",      "device_id": "..." }
```
Client → server:
```jsonc
{ "type": "hello",     "device_id": "...", "last_seq": { "game_id": seq, ... } }
{ "type": "subscribe", "games": ["*"] }     // * = all games on account
{ "type": "ping" }
```
- On `hello`, server replays any `version_available` the device missed while offline (using `last_seq` per game). This is the "catch up on reconnect" from PRD-03 §5.
- Heartbeat: `ping`/`pong` every 30 s; drop + mark offline after miss.

## 5. Handler sketch (Axum)

```rust
async fn create_version(
    State(app): State<AppState>,
    Auth(device): Auth,                 // extractor validates device JWT
    Path(game_id): Path<GameId>,
    Json(req): Json<CreateVersion>,     // from savr-core
) -> Result<impl IntoResponse, ApiError> {
    if !app.blobs.exists(&req.blob_hash).await? {
        return Err(ApiError::blob_missing(req.blob_hash));
    }
    match app.store.try_advance_head(&device.account, game_id, req).await? {
        Advance::FastForward(v) => {
            app.hub.broadcast_except(&device, VersionAvailable::from(&v)).await;
            Ok((StatusCode::CREATED, Json(v)))
        }
        Advance::Conflict { head, incoming } => {
            app.hub.notify(&device.account, Conflict::new(&head, &incoming)).await;
            Err(ApiError::conflict(head, incoming))
        }
    }
}
```
`try_advance_head` is a single DB transaction: check `parent == head`, insert version, conditionally update head. Compare-and-swap prevents races between two devices uploading at once.

## 6. Performance targets

| Path | Target |
|---|---|
| `POST /versions` (blob already present) | < 20 ms p99 |
| Blob `PUT` throughput | disk/network bound, streamed (no full buffering) |
| WS push fan-out | < 100 ms server-side |
| Idle server RAM | < 60 MB (SQLite mode) |

Stream blobs (never buffer whole archives in memory), use `sqlx` prepared statements, keep the hot path (`create_version`) to one transaction.
