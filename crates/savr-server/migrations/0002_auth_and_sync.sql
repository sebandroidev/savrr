-- 0002: real auth (pairing codes) + conflict-resolution audit (PRD-06, PRD-03).
-- 0001 stays unchanged. FK enforcement remains off (see 0001's note); integrity
-- is upheld by the CAS head-advance, the blob-exists check, and refcount GC.

-- One-time device pairing codes (PRD-06 §2). Codes are stored HASHED (argon2);
-- the plaintext is shown once at generation and never persisted. Single-use via
-- the `used` flag, short-lived via `expires_at`.
CREATE TABLE pairing_codes (
    id          TEXT PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    code_hash   TEXT NOT NULL,       -- argon2 of the one-time code
    expires_at  TEXT NOT NULL,       -- RFC3339, TTL 5 min
    used        INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_pairing_active ON pairing_codes(used, expires_at);

-- Audit of resolved conflicts (PRD-03 §4). Recording the loser lets retention
-- know that branch tip is safe to prune (it is no longer an *unresolved* tip),
-- and `redirect` carries the "keep both" sibling-folder note for restore.
CREATE TABLE resolved_conflicts (
    id           TEXT PRIMARY KEY,
    account_id   TEXT NOT NULL,
    game_id      TEXT NOT NULL,
    winner       TEXT NOT NULL,      -- version id that became head
    loser        TEXT NOT NULL,      -- version id, kept as a branch (never deleted here)
    keep_both    INTEGER NOT NULL DEFAULT 0,
    redirect     TEXT,               -- sibling-folder note when keep_both = 1
    resolved_at  TEXT NOT NULL
);
CREATE INDEX idx_resolved_game ON resolved_conflicts(account_id, game_id);
