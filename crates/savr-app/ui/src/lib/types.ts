// TypeScript mirror of the savr-core wire types that cross the IPC boundary.
// These match the serde JSON shapes exactly (snake_case enums, hex hashes).

export type Uuid = string;
export type IsoDateTime = string;

export type Os = "windows" | "linux" | "macos";
export type SaveTag = "save" | "config";
export type GameSource = "Manifest" | "Steam" | "Custom";
export type VersionKind = "full" | "differential";
export type RootKind = "steam" | "drive" | "emulator" | "launcher";
export type ResolveChoice = "keep_mine" | "keep_theirs" | "keep_both";
export type ConflictPolicy =
  | "manual"
  | "latest_wins"
  | "theirs_wins"
  | "mine_wins";
export type AutoPullPolicy = "ask" | "auto";

export interface SaveTarget {
  glob: string;
  tags: SaveTag[];
  os_hint: Os | null;
  registry: boolean;
}

export interface Game {
  id: Uuid;
  title: string;
  source: GameSource;
  steam_appid: number | null;
  save_targets: SaveTarget[];
  // Detection/play stats overlaid by the daemon (see savr-core Game).
  running: boolean;
  last_played: IsoDateTime | null;
  last_session_secs: number | null;
  total_secs: number;
}

export interface FileEntry {
  rel_path: string;
  size: number;
  mtime: number;
  hash: string;
}

export interface Version {
  id: Uuid;
  game_id: Uuid;
  device_id: Uuid;
  parent: Uuid | null;
  kind: VersionKind;
  files: FileEntry[];
  blob_hash: string;
  bytes: number;
  seq: number;
  created_at: IsoDateTime;
}

export interface Root {
  id: Uuid;
  kind: RootKind;
  path: string;
}

export interface RootSpec {
  kind: RootKind;
  path: string;
}

export interface DaemonStatus {
  version: string;
  uptime_s: number;
  rss_bytes: number;
  watched_games: number;
  server_connected: boolean;
  last_backup_at: IsoDateTime | null;
  pending_outbox: number;
  autostart_enabled: boolean;
}

export interface Retention {
  full: number;
  diff_per_full: number;
}

export interface PathOverride {
  game_id: Uuid;
  globs: string[];
}

export interface SyncedConfig {
  tag: string;
  custom_games: Game[];
  overrides: PathOverride[];
  conflict_policy: ConflictPolicy;
  autopull_policy: AutoPullPolicy;
  retention: Retention;
}

// Structured error shape returned by every command (see error.rs).
export interface CmdError {
  kind: "daemon_unreachable" | "daemon" | "protocol" | "io";
  message: string;
}

export function errorMessage(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) {
    return String((e as CmdError).message);
  }
  return String(e);
}

export function isDaemonDown(e: unknown): boolean {
  return (
    !!e &&
    typeof e === "object" &&
    "kind" in e &&
    (e as CmdError).kind === "daemon_unreachable"
  );
}
