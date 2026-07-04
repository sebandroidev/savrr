# PRD-05 · Data Models

Three surfaces, one source of truth (`savr-core`): wire types (JSON), server DB (SQL), and local daemon state.

---

## 1. Core Rust types (`savr-core`)

```rust
pub type GameId    = Uuid;   // v7
pub type VersionId = Uuid;   // v7
pub type DeviceId  = Uuid;
pub type AccountId = Uuid;
pub type Blake3Hash = [u8; 32]; // hex in JSON

#[derive(Serialize, Deserialize, Clone)]
pub struct Game {
    pub id: GameId,
    pub title: String,           // canonical (PCGamingWiki) title
    pub source: GameSource,      // Manifest | Custom
    pub steam_appid: Option<u32>,
    pub save_targets: Vec<SaveTarget>, // resolved-independent path templates
}

#[derive(Serialize, Deserialize, Clone)]
pub enum GameSource { Manifest, Custom }

#[derive(Serialize, Deserialize, Clone)]
pub struct SaveTarget {
    pub glob: String,            // with placeholders, pre-resolution
    pub tags: Vec<SaveTag>,      // Save | Config
    pub os_hint: Option<Os>,     // treat as hint, not filter (PRD-02 §1.2)
    pub registry: bool,          // Windows registry key vs filesystem
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileEntry {
    pub rel_path: String,
    pub size: u64,
    pub mtime: i64,
    pub hash: Blake3Hash,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Version {
    pub id: VersionId,
    pub game_id: GameId,
    pub device_id: DeviceId,
    pub parent: Option<VersionId>,
    pub kind: VersionKind,       // Full | Differential
    pub files: Vec<FileEntry>,
    pub blob_hash: Blake3Hash,
    pub bytes: u64,
    pub seq: u64,                // server-assigned, per (account, game)
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum VersionKind { Full, Differential }

#[derive(Serialize, Deserialize, Clone)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,            // "Desktop", "Steam Deck"
    pub os: Os,
    pub last_seen: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum Os { Windows, Linux, Macos }

/// Account-level config, synced to every device (PRD-04 /config).
#[derive(Serialize, Deserialize, Clone)]
pub struct SyncedConfig {
    pub tag: String,                       // optimistic concurrency
    pub custom_games: Vec<Game>,           // user-defined
    pub overrides: Vec<PathOverride>,      // redirects for manifest games
    pub conflict_policy: ConflictPolicy,   // Manual | LatestWins | ...
    pub autopull_policy: AutoPullPolicy,   // Ask | Auto
    pub retention: Retention,              // N full + M diff
}
```

Wire types are these structs, serialized with `serde_json`. Hashes serialize as hex strings. Datetimes as RFC 3339.

## 2. Server DB schema (SQLite default / Postgres compatible)

```sql
CREATE TABLE accounts (
    id          BLOB PRIMARY KEY,       -- uuid
    owner_hash  TEXT NOT NULL,          -- argon2 of owner password
    created_at  TEXT NOT NULL
);

CREATE TABLE devices (
    id          BLOB PRIMARY KEY,
    account_id  BLOB NOT NULL REFERENCES accounts(id),
    name        TEXT NOT NULL,
    os          TEXT NOT NULL,
    token_hash  TEXT NOT NULL,          -- hash of device refresh secret
    last_seen   TEXT,
    revoked     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE games (
    id          BLOB PRIMARY KEY,
    account_id  BLOB NOT NULL REFERENCES accounts(id),
    title       TEXT NOT NULL,
    steam_appid INTEGER,
    head        BLOB REFERENCES versions(id),  -- nullable until first backup
    UNIQUE(account_id, title)
);

CREATE TABLE versions (
    id          BLOB PRIMARY KEY,
    game_id     BLOB NOT NULL REFERENCES games(id),
    account_id  BLOB NOT NULL,
    device_id   BLOB NOT NULL REFERENCES devices(id),
    parent      BLOB REFERENCES versions(id),
    kind        TEXT NOT NULL,          -- 'full' | 'diff'
    blob_hash   TEXT NOT NULL REFERENCES blobs(hash),
    files_json  TEXT NOT NULL,          -- serialized Vec<FileEntry>
    bytes       INTEGER NOT NULL,
    seq         INTEGER NOT NULL,       -- per (account, game)
    created_at  TEXT NOT NULL,
    UNIQUE(game_id, seq)
);
CREATE INDEX idx_versions_game ON versions(game_id, seq DESC);

CREATE TABLE blobs (
    hash        TEXT PRIMARY KEY,       -- blake3 hex
    bytes       INTEGER NOT NULL,
    refcount    INTEGER NOT NULL,       -- for GC
    created_at  TEXT NOT NULL
);

CREATE TABLE config (
    account_id  BLOB PRIMARY KEY REFERENCES accounts(id),
    tag         TEXT NOT NULL,
    data_json   TEXT NOT NULL           -- serialized SyncedConfig
);
```

Head advance is a CAS in one transaction:
```sql
UPDATE games SET head = :new WHERE id = :game AND head IS :expected_parent;
-- rows_affected == 0  → conflict
```

## 3. Local daemon state (per device, SQLite)

```sql
CREATE TABLE local_snapshots (      -- last snapshot per game, for diffing
    game_id     BLOB PRIMARY KEY,
    files_json  TEXT NOT NULL,
    registry    BLOB,               -- windows only
    taken_at    TEXT NOT NULL,
    local_head  BLOB                 -- last version id this device knows
);

CREATE TABLE exe_index (            -- exe -> game map (PRD-02 §3.2)
    exe_key     TEXT PRIMARY KEY,   -- canonical path or basename
    game_id     BLOB NOT NULL,
    confidence  INTEGER NOT NULL    -- path-match > basename-match
);

CREATE TABLE outbox (               -- queued uploads when server offline
    version_id  BLOB PRIMARY KEY,
    payload     BLOB NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    next_retry  TEXT
);

CREATE TABLE roots (                -- registered folders (PRD-02 §2)
    id          BLOB PRIMARY KEY,
    kind        TEXT NOT NULL,      -- steam | drive | emulator | launcher
    path        TEXT NOT NULL
);
```

## 4. Local IPC schema (daemon ↔ GUI)

Length-prefixed JSON frames over unix socket / named pipe.

```rust
// GUI -> daemon
enum GuiRequest {
    ListGames,
    ListRoots, AddRoot(RootSpec), RemoveRoot(Uuid),
    BackupNow(GameId),
    ListVersions(GameId),
    Restore { game_id: GameId, version_id: VersionId },
    ResolveConflict { game_id: GameId, choice: ResolveChoice },
    GetStatus,               // daemon health, RAM, current watched games
    UpdateConfig(SyncedConfig),
    EnterLearnMode(GameId),  // capture exe (PRD-02 §3.3)
}
// daemon -> GUI (responses + events)
enum DaemonMsg {
    Games(Vec<Game>),
    Versions(Vec<Version>),
    Status(DaemonStatus),
    Event(DetectionEvent),   // live feed for the GUI
    ConflictRaised { game_id: GameId, tips: [VersionId; 2] },
    Ok, Error(String),
}
```

## 5. On-disk archive format (`.savr`)

```
<zstd stream>
 └── tar:
      meta.json        # Version (serde)
      files/<rel_path> # changed file bytes (diff) or all (full)
      deletions.json   # rel_paths removed since last full (diff only)
      registry.json    # HKCU export (windows only)
```
`blob_hash = blake3(entire .savr byte stream)` → content address + integrity.
