-- Server schema (PRD-05 §2). UUIDs stored as hyphenated TEXT (simpler than BLOB
-- round-tripping; swap to BLOB later if the size matters). Foreign keys ARE
-- enforced: sqlx's SqliteConnectOptions turns on `PRAGMA foreign_keys` per
-- connection, so e.g. a version can never reference a missing blob or game.

CREATE TABLE accounts (
    id          TEXT PRIMARY KEY,
    owner_hash  TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE devices (
    id          TEXT PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    name        TEXT NOT NULL,
    os          TEXT NOT NULL,
    token_hash  TEXT NOT NULL,
    last_seen   TEXT,
    revoked     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE blobs (
    hash        TEXT PRIMARY KEY,
    bytes       INTEGER NOT NULL,
    refcount    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
);

CREATE TABLE games (
    id          TEXT PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    title       TEXT NOT NULL,
    steam_appid INTEGER,
    head        TEXT REFERENCES versions(id),
    UNIQUE(account_id, title)
);

CREATE TABLE versions (
    id          TEXT PRIMARY KEY,
    game_id     TEXT NOT NULL REFERENCES games(id),
    account_id  TEXT NOT NULL,
    device_id   TEXT NOT NULL REFERENCES devices(id),
    parent      TEXT REFERENCES versions(id),
    kind        TEXT NOT NULL,           -- 'full' | 'diff'
    blob_hash   TEXT NOT NULL REFERENCES blobs(hash),
    files_json  TEXT NOT NULL,
    bytes       INTEGER NOT NULL,
    seq         INTEGER NOT NULL,        -- per (account, game)
    created_at  TEXT NOT NULL,
    UNIQUE(game_id, seq)
);
CREATE INDEX idx_versions_game ON versions(game_id, seq DESC);

CREATE TABLE config (
    account_id  TEXT PRIMARY KEY REFERENCES accounts(id),
    tag         TEXT NOT NULL,
    data_json   TEXT NOT NULL
);
