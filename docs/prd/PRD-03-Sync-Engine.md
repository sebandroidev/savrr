# PRD-03 · Sync Engine

Turns "a game stopped" into a versioned, deduplicated, conflict-safe backup on the server, and turns "new version available" into a safe restore on another device.

---

## 1. Snapshot

A **snapshot** is the current on-disk state of one game's save set.

```rust
struct FileEntry {
    rel_path: String,     // relative to a stable save-root anchor
    size: u64,
    mtime: i64,
    hash: Blake3Hash,     // content hash
}
struct Snapshot {
    game_id: GameId,
    files: Vec<FileEntry>,      // sorted by rel_path for determinism
    registry: Option<RegistryBlob>, // Windows only
    taken_at: DateTime,
}
```

Building it:
1. Resolve save paths (manifest + manual, PRD-02).
2. Walk globs → concrete files.
3. Hash each file with **blake3** (fast; parallel with `rayon`).
4. Read HKCU registry keys (Windows) into `RegistryBlob`.

Keep the last snapshot per game on disk (`state db`, PRD-05) so the next backup can diff against it without re-reading the server.

## 2. Diff + package

Compare new snapshot to last known snapshot:
- **Unchanged** (same file set + hashes + registry) → no-op, skip upload. (Critical for G5/G6 – don't churn.)
- **Changed** → build a **version**.

Two archive modes (config, default = differential like Ludusavi):
- **Full:** zstd archive of all files in the snapshot.
- **Differential:** zstd archive of only files whose hash changed since the last *full*, plus a deletions list. Periodically (every K diffs or size threshold) take a fresh full to cap restore chain length.

Archive format: a single `.savr` file = zstd-compressed tar-like container holding: `meta.json` (the `Version` metadata), changed file blobs, and `registry.json` if present. Compute `blob_hash = blake3(archive_bytes)` for content addressing/dedup.

```rust
struct Version {
    id: VersionId,           // uuid
    game_id: GameId,
    device_id: DeviceId,
    parent: Option<VersionId>, // the head this device backed up from
    kind: Full | Differential,
    files: Vec<FileEntry>,   // full logical file set (for restore + duplicate detection)
    blob_hash: Blake3Hash,   // the .savr archive
    bytes: u64,
    created_at: DateTime,
}
```

## 3. Upload

1. `POST /games/{id}/versions` with `Version` metadata + `parent = local_head`.
2. Server checks: does it already have `blob_hash`? If yes, skip the blob transfer (dedup). If no, `PUT` the archive to the blob endpoint (resumable for large saves).
3. Server response either **accepts and advances head**, or returns **409 Conflict** with the current server head (see §4).

## 4. Conflict model (never silently overwrite)

The server keeps a per-account **head** version id per game. Think Git-lite:

- Device backs up with `parent = last head it saw`.
- If `parent == server.head` → fast-forward: accept, `head = new version`. ✅
- If `parent != server.head` → someone else advanced it → **conflict**. Server stores the new version as a **divergent branch** (does not move head), returns `409` + both tips.

Resolution (config policy, default = `manual`):
| Policy | Behavior |
|---|---|
| `manual` (default) | Daemon notifies; GUI shows a resolve view: **Keep mine / Keep theirs / Keep both**. Loser is preserved as a version (never deleted), so nothing is lost. |
| `latest_wins` | Higher `created_at` becomes head; loser kept as branch + a notification. |
| `theirs_wins` / `mine_wins` | Fixed preference. |

The resolve view surfaces per-file info (size, mtime) and reuses Ludusavi's "duplicate/conflict" idea: show which files differ so the user can choose confidently. "Keep both" writes a redirect so the loser restores to a sibling folder.

**Safety invariant:** a backup is only ever *additive* on the server. Restores are the only destructive local action, and they always snapshot-then-restore (see §6).

## 5. Notify → pull

On head advance, server pushes `VersionAvailable { game_id, version_id }` over WebSocket to the account's other online devices (PRD-04 §4). Offline devices get it on reconnect (server sends "since last seen" deltas).

Receiving daemon:
- **auto-pull policy = ask (default):** notify "New save for *Game X*. Download?" → user accepts → restore.
- **auto-pull policy = auto:** pull + restore immediately **iff** the local game isn't running and local saves are unchanged since last sync (else fall back to ask, to avoid clobbering unsynced local progress).

## 6. Restore (safe by construction)

1. Refuse if the game is currently running (would corrupt live save).
2. **Pre-restore snapshot:** back up current local saves as a version first (so restore is undoable).
3. Download the target version's archive (+ its differential chain back to the last full).
4. Reconstruct the file set; write atomically (write to temp dir, then swap) to the resolved save paths.
5. Restore registry keys (Windows).
6. Update local head + last snapshot to the restored version.

## 7. Retention

Configurable per account (like Ludusavi's full+diff retention): keep last **N full** + **M differential per full**. When a full is pruned, its differentials prune with it. Server GC deletes orphaned blobs (no version references them) on a schedule. Never prune the current head or an unresolved conflict tip.

## 8. Failure handling

- Upload interrupted → resumable blob PUT; version metadata only commits after blob is fully stored.
- Server unreachable → queue versions locally (`outbox`), retry with backoff; daemon keeps latest local snapshot so nothing is lost.
- Corrupt archive on restore → verify `blob_hash` before writing; abort + report if mismatch.
- Clock skew → order by server-assigned sequence, not client wall clock, for head decisions.
