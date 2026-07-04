# Known issues

Savrr is early. Before the first public commit it went through an adversarial code review across five areas: save-data integrity, server-side history, auth and access control, concurrency, and wire-protocol correctness. This is what that review found, what got fixed, and what's still open. If you're deciding whether to trust Savrr with a save, read the open list first.

## Fixed before release

- **Revoked device kept a live WebSocket.** Deleting a device blocked its REST access but left its push channel open, and the device could re-register over it. Now the socket re-checks revocation each heartbeat and refuses to re-register, so a revoked device is cut off within ~30 seconds. (`savr-server`)
- **Offline differential backups could be inconsistent.** A second backup taken while the server was unreachable declared a parent version the server had never advanced to, which could produce a differential that wouldn't restore correctly. Offline backups after the first are now self-contained full backups until the server confirms one. A crash mid-queue no longer loses the change either. (`savr-daemon`)
- **Config edits stopped syncing after the first.** The daemon dropped the new concurrency tag the server returned on a config update, so the next edit was rejected. It now adopts the server's tag. (`savr-daemon`)
- **Manual "Back Up Now" reported a normal conflict as an error.** A manual backup that diverged from the server surfaced a protocol error in the GUI instead of a conflict. (`savr-app`)
- **`/auth/refresh` leaked device state.** Three distinct error messages let someone with a device id probe whether it existed and whether it was revoked. Collapsed to one generic response. (`savr-server`)

## Open

These are real but bounded. None of them silently destroy your only copy of a save, but they're worth knowing.

- **No per-game lock between a backup and a restore (medium).** If a backup and a restore of the *same* game run at the same time, the backup can read a half-restored folder and upload an inconsistent version. In practice this needs two triggers to overlap on one game. The safe fix is a serialization lock; it's deliberately not in yet because it needs careful testing to avoid a deadlock with the pre-restore backup step.
- **Restore is per-file atomic, not whole-set atomic (medium).** Each file is swapped in atomically, but a crash partway through leaves some files new and some old. The restore always snapshots the current state first, so it's recoverable by restoring again, but the intermediate state is inconsistent.
- **A slow WebSocket client can grow server memory (medium).** The per-device push queue is unbounded. A device that connects but stops reading accumulates queued messages. A bounded channel that drops the slow client is the fix.
- **Version sequence numbers can be reused after GC (low/medium).** The server derives the next sequence number from the surviving rows, so pruning the highest-numbered version can reuse its number. A device reconnecting could miss that one version in its catch-up (it still gets the latest). A monotonic counter fixes it.
- **Symlinked save files are skipped (low).** The snapshotter walks real files and directories; a save that is itself a symlink is neither backed up nor restored.
- **A large restore can delay WebSocket heartbeats (low).** The daemon handles a server push inline, so a long restore can stall pings and briefly look disconnected.
- **Concurrent backups of one game can 500 instead of 409 (low).** Two devices backing up the same game at the exact same moment can hit a database write conflict that surfaces as a server error rather than a clean conflict; the client retries.

Found something not listed here? Open an issue. For anything security-sensitive, see [SECURITY.md](../SECURITY.md).
