# Non-Steam games — Phase 1 design

Status: approved (design), pending implementation plan.
Date: 2026-07-08.
Scope: Phase 1 of a 3-phase effort to support games not installed through Steam
(pirated, GOG/Epic/itch, hand-installed). Phases 2 (real Learn mode) and 3
(launcher-specific auto-config) are out of scope here and build on this
foundation.

## Problem

Today the catalog is built exclusively by scanning Steam libraries on every
launch (`Engine::refresh_games`, `crates/savr-daemon/src/engine.rs`). Games live
in memory only — there is **no games table** — so anything not re-derivable from
a Steam scan disappears on restart. Detection (`ExeIndex`) only indexes exes
found under Steam install dirs. Save locations come from the Ludusavi manifest
keyed by Steam appid. "Learn mode" is a stub that records intent and does nothing.

A non-Steam game therefore cannot be (a) listed, (b) detected as running, or
(c) backed up. `GameSource::Custom` exists as a type but nothing produces it;
`RootKind::Drive` exists but is never consumed.

## How other tools solve identity (research)

Ludusavi identifies a game from its **install-folder name**, not its store: it
scans "roots" (folders containing game install dirs) and fuzzy-matches each
subfolder name against the manifest's `installDir` keys and the game title,
picking the best match. This is why many tools "just detect" a game and its save
info regardless of how it was installed.

Two facts make this cheap for us:

- Our manifest parser **already keeps the `installDir` folder-name keys**
  (`ManifestEntry::install_dir: BTreeMap<String, IgnoredAny>`,
  `crates/savr-core/src/manifest.rs:22`). The keys are the folder names; we
  ignore only their values.
- The `<base>` placeholder **already resolves to a game's install dir**
  (`manifest.rs` `resolve`). A manifest-matched game installed anywhere gets its
  save paths resolved for free once we supply the install dir as `base`.

Sources: ludusavi-manifest README; ludusavi `docs/help/roots.md`; ludusavi issue
#434 (detect manually-installed games).

## Cross-device identity (the answer to "how do devices agree?")

Match by **canonical name**, consistent with how Steam games already agree
(currently by appid via `ensure_game(title, Some(appid))`).

- Auto-detected games carry the manifest's canonical title, so two devices that
  match the same manifest entry agree automatically.
- Manual games are matched by their user-assigned, normalized title.

Generalize `game_id_for` to key by name when there is no appid: meta key
`gameid:name:<normalized_title>` and `ensure_game(title, None)`. Normalization =
lowercase, trim, collapse whitespace, strip surrounding punctuation (same
routine used by fuzzy matching, below). Existing Steam identity
(`gameid:steam:<appid>`) is unchanged.

## Design

Two capabilities on one shared foundation.

### Capability A — auto-detect (Ludusavi-style)

The user adds a **game-folder root**: any folder that contains game install dirs
(`D:\Games`, a GOG/Epic library, etc.). Reuse the dormant `RootKind::Drive` for
this (rename its user-facing label to "Game folder"; the stored discriminant
string `"drive"` stays for back-compat).

On `refresh_games`, for each `Drive` root:

1. List immediate subfolders (each is a candidate install dir). One level deep
   only in Phase 1 — no recursive descent.
2. Normalize the subfolder name and match it against (a) every manifest entry's
   `installDir` keys and (b) the manifest title. Matching is **exact on the
   normalized string** in Phase 1 (high precision). A similarity-ratio pass is a
   noted future refinement.
3. On a unique match: build a `Game { source: Manifest, steam_appid: None,
   save_targets: entry.save_targets(), title: <canonical> }`, resolve its save
   paths with the subfolder as `<base>`, and index the subfolder's exes for
   detection (`ExeIndex::index_install_dir`, already exists).
4. On **no match or an ambiguous match (>1 distinct game), skip it** — never
   guess. A wrong match backs up the wrong folder, which is worse than nothing;
   the user can still add it manually.

This is almost entirely wiring over existing machinery (manifest lookup, resolve,
exe index, name-based identity). No new save format, no new pipeline.

### Capability B — manual add (the long tail)

For games not in the manifest, the user adds one by hand with:

- `title` (used for cross-device identity)
- `install_path` — folder or single exe, for detection. Folder → index all exes
  in it; exe → index that exe plus its parent dir.
- `save_root` — absolute folder the saves live in
- `include` globs (default `**/*` = whole folder) and `exclude` globs
  (e.g. `logs/**`, `*.tmp`)

Persist in a new table (Steam/auto-detected games are re-derived each launch; a
manual entry has nowhere else to come from):

```sql
CREATE TABLE IF NOT EXISTS custom_games (
    id           TEXT PRIMARY KEY,   -- GameId (uuid)
    title        TEXT NOT NULL,
    install_path TEXT,               -- nullable: detection optional
    save_root    TEXT NOT NULL,
    include_glob TEXT NOT NULL,      -- newline-joined patterns
    exclude_glob TEXT NOT NULL,      -- newline-joined patterns
    created_at   TEXT NOT NULL
);
```

Save resolution for a manual game builds snapshot **patterns** from
`save_root` + `include`, with `exclude` applied as a filter. The backup pipeline
is already glob-native — `resolve_game` yields `patterns` + `anchor` and
`Snapshot::build(game_id, &patterns, &anchor)` walks them
(`crates/savr-daemon/src/backup.rs:70`). New work: an exclude filter, since
today's patterns are include-only. Excludes are applied during snapshot walk (or
as a post-filter on the built file list if that is simpler and equivalent).

### Shared foundation

- **`custom_games` table** + `LocalState` methods: `add_custom_game`,
  `remove_custom_game`, `list_custom_games`.
- **`refresh_games` extended**: after Steam libs → scan `Drive` roots
  (Capability A) → load `custom_games` (Capability B). All three sources merge
  into the single `self.games` catalog and the one rebuilt `ExeIndex`. On a
  title collision between sources, prefer in order: Steam-manifest > auto-detect
  > manual (a manual entry for something already found should not duplicate it).
- **`game_id_for` generalized** to name-based identity when appid is absent
  (see Cross-device identity).
- **IPC additions** (`crates/savr-core/src/ipc.rs`, `GuiRequest`): struct
  variants only, per the internal-tagging constraint. `AddCustomGame { ... }`,
  `RemoveCustomGame { id }`. Reuse existing `AddRoot`/`RemoveRoot` for the
  game-folder root (already `Drive`-capable) and existing catalog refresh to
  rescan. Add a `DaemonMsg` reply only if a new shape is needed; `Ok` suffices
  for adds/removes.
- **App commands + UI**: an "Add game folder" action (reuses the roots flow) and
  an "Add game" form (title, install path, save folder, include/exclude globs)
  in the Games view, plus a way to remove a manual game.

## Data flow

Add game folder → `AddRoot(Drive)` → `refresh_games` → scan subfolders →
manifest match → catalog + exe index → detection + backup use the existing
Steam-game path.

Add manual game → `AddCustomGame` → row in `custom_games` → `refresh_games`
loads it → resolve `save_root`+globs to patterns/anchor → same backup pipeline.

Detection and backup downstream of the catalog are **unchanged**; a non-Steam
game is just another entry in `self.games` and another set of rows in the exe
index.

## Error handling

- Unreadable / missing root folder → log and skip that root; never fail the whole
  refresh (matches existing catalog-resilience behavior).
- Ambiguous or absent manifest match → skip (Capability A) or rely on manual add.
- Manual game with an unreadable `save_root` → list the game but a backup reports
  "save location unavailable" rather than erroring the catalog.
- Duplicate manual add (same normalized title) → reject with a clear message.
- Name-based `ensure_game` failure when paired → fall back to a cached/local id,
  exactly as the appid path already does (`game_id_for` is infallible by design).

## Testing

- Fuzzy/exact match: folder name → correct manifest entry; ambiguous (two games
  share a normalized installDir) → `None`; unknown folder → `None`.
- Title normalization roundtrip (used by both matching and identity).
- `custom_games` persistence roundtrip (add / list / remove).
- Glob resolution: include-only, include+exclude, exclude removes the right files.
- `refresh_games` merge: Steam + one auto-detected + one manual game all appear
  once; a manual entry duplicating an auto-detected title collapses per the
  precedence rule.
- Name-based `game_id_for`: stable across calls; unchanged appid behavior.

## Out of scope (later phases)

- Phase 2: make Learn mode real — discover `save_root` by watching a play session
  instead of asking the user.
- Phase 3: launcher-specific auto-config (read GOG/Epic/Heroic manifests to add
  roots and store-game-ids automatically).
- Similarity-ratio fuzzy matching (Phase 1 is exact-normalized only).
- Recursive multi-level root scanning.
```
