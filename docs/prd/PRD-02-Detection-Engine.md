# PRD-02 · Detection Engine

The hardest part of the product: knowing (a) **where** a game stores its saves, and (b) **when** a game starts and stops. Split into four subsystems.

---

## 1. Games database (the "where")

### 1.1 Source: Ludusavi Manifest
Do not hand-maintain save paths. Import the **Ludusavi Manifest** – a YAML database of save locations compiled from PCGamingWiki + the Steam API, designed to be reused by any backup tool.

- **Fetch:** `GET https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml`
- **Update check:** store the `ETag`; re-request with `If-None-Match`. `304` = no change, `200` = new manifest → replace cache. Do this on daemon start + daily.
- **Cache:** `manifests/manifest.yaml` + stored ETag. Parse into an in-memory index keyed by normalized game title.

### 1.2 Manifest entry shape (subset we consume)
```yaml
An Example Game:
  files:
    "<base>/saves":
      tags: [save]
    "<base>/settings.json":
      when: [{ os: windows }, { os: linux }]
      tags: [config]
  installDir:
    AnExampleGame: {}
  registry:
    "HKEY_CURRENT_USER/Software/An Example Game":
      tags: [save, config]
  steam:
    id: 123
```

Key behaviors to replicate:
- Paths are **glob patterns** after placeholder substitution; a path may match many files/dirs; folders back up recursively.
- `when: [{os: ...}]` marks likely-OS, but **still check other OSes** – the dataset often only records one confirmed location. Treat `when` as a hint, not a hard filter.
- `registry:` entries (Windows only) must be captured too (HKCU keys, recursive).
- **Secondary manifests:** if a file named `.ludusavi.yaml` exists directly in a game's install dir, merge it in (devs can ship their own).

### 1.3 Path placeholders
Resolve these against the current OS + registered roots before globbing:

| Placeholder | Resolves to |
|---|---|
| `<base>` | the game's install/base dir under a root (store-specific) |
| `<home>` | user home dir |
| `<winAppData>` | `%APPDATA%` |
| `<winLocalAppData>` | `%LOCALAPPDATA%` |
| `<winDocuments>` | Documents |
| `<xdgData>` | `$XDG_DATA_HOME` (`~/.local/share`) |
| `<xdgConfig>` | `$XDG_CONFIG_HOME` (`~/.config`) |
| `<storeUserId>` / `<osUserName>` | Steam/OS user id |

Implement resolution in `savr-core::manifest::resolve()` so daemon, GUI, and server agree.

### 1.4 Manual override (always available)
The DB will not cover every game. The GUI lets the user:
- Add a **custom game** with explicit save paths/globs.
- **Override** a DB entry's paths (redirect), for non-standard installs.
- Map a specific executable to a game (see §3.3).
Custom entries are stored account-side and synced to all devices (they are part of user config, PRD-05).

---

## 2. Roots (where to look)

A **root** is a folder the user registers. Placeholder `<base>` and generic scans are evaluated relative to roots.

Root types (v1):
- **Steam library** – parse `steamapps/libraryfolders.vdf` + per-game `appmanifest_*.acf` for `installdir` and `appid`.
- **Generic drive / folder** – e.g. `D:\Games`, `/mnt/games`.
- **Emulator data dir** – e.g. RetroArch saves, PCSX2 memcards.
- **Launcher roots (later):** Heroic (GOG/Epic), Lutris/Proton prefixes.

For Steam + Proton on Linux, save data may live in the Proton prefix `compatdata/<appid>/pfx/drive_c/...`; resolve `<winAppData>` etc. inside the prefix, mirroring how Ludusavi handles Proton.

---

## 3. Process watching (the "when")

### 3.1 Poller
The daemon runs a lightweight loop using `sysinfo`:
```rust
// pseudocode – savr-daemon::watcher
let mut sys = System::new();
loop {
    sys.refresh_processes();
    let running: HashSet<ExeKey> = sys.processes()
        .values()
        .filter_map(|p| exe_key(p))     // canonical path or basename
        .collect();

    for started in running.difference(&last) {
        if let Some(game) = index.match_exe(started) {
            emit(GameStarted { game, pid, at: now() });
        }
    }
    for stopped in last.difference(&running) {
        if let Some(game) = index.match_exe(stopped) {
            schedule_backup(game, debounce = settle_ms); // e.g. 3–8s
        }
    }
    last = running;
    sleep(poll_interval); // 2–5s; adaptive (slower when idle)
}
```

- **Poll interval:** 2–5 s default; back off to 10–15 s when no known game has been seen recently to keep CPU ~0. Speed back up when any game process appears.
- **Debounce on stop:** wait `settle_ms` after the exe disappears so the game finishes flushing its save file before we read it.
- **Debounce on start:** ignore flaps (launcher spawning/killing helper processes).

### 3.2 Building the exe → game index
Populate a map from executable identity to `game_id`:
1. From Steam `appmanifest` `installdir` + manifest `installDir` names, find the game's install folder under each root; index the executables found there.
2. From PCGamingWiki data where available (some entries hint at binary names).
3. From **user launches** – if the user launches a game *through* Savr (see §3.4), we learn its exe with certainty and cache it.
4. From **manual mapping** in the GUI.

Match strategy: prefer full canonical exe **path** match (exe under a known game dir under a known root). Fall back to **basename** match only when unambiguous, to avoid `game.exe` collisions.

### 3.3 Manual mapping UX
GUI: "This game isn't detected? Play it once with detection in learn-mode, and we'll capture the executable." Learn-mode records new exes that appear/disappear while the user confirms which is the game.

### 3.4 Launch-through (most reliable signal, optional)
Like Steam/Playnite: let the user launch a game *from Savr*. We spawn the process ourselves → exact start, and we get an exact stop event on child exit. This sidesteps polling ambiguity entirely for launched games. Offer it, but never require it (G1 wants zero-touch for games launched normally too).

---

## 4. Platform-specific notes

### Windows
- Enumerate processes via `sysinfo`; canonical exe paths available.
- **Registry saves:** back up `registry:` HKCU keys → export to a `.reg`-equivalent structure inside the version archive; restore writes them back. Use the `windows` crate / `winreg`.
- Common save roots: `%APPDATA%`, `%LOCALAPPDATA%`, `Documents\My Games`, `Saved Games`.
- Service: run daemon as a Windows Service (or Scheduled Task at logon for user-scope registry access – note HKCU needs the user's session).

### Linux
- Processes via `sysinfo` / `/proc`.
- Saves: `~/.local/share`, `~/.config`, and **Proton prefixes** under `steamapps/compatdata/<appid>/pfx/`.
- Steam "add non-Steam game": match by the title Savr knows the game as.
- Service: `systemd --user` unit (user session, correct home paths). Tray needs `libayatana-appindicator`.

### macOS
- Processes via `sysinfo`.
- Saves: `~/Library/Application Support`, `~/Library/Containers/<bundle>/Data/...` (sandboxed apps), `~/Library/Preferences`.
- Service: `launchd` LaunchAgent (per-user).
- Note: sandboxed App Store games hide saves inside `Containers`; resolve by bundle id where known.

---

## 5. Detection engine outputs (events → sync engine)
```rust
enum DetectionEvent {
    GameStarted { game_id: GameId, pid: u32, at: DateTime },
    GameStopped { game_id: GameId, at: DateTime },   // debounced
    ManualBackupRequested { game_id: GameId },
    SaveDirChanged { game_id: GameId },              // from `notify`, optional live backup
}
```
`GameStopped` and `ManualBackupRequested` trigger a backup in the sync engine (PRD-03). `SaveDirChanged` can optionally trigger mid-session incremental backups for long play sessions (config: off by default to protect G5).
