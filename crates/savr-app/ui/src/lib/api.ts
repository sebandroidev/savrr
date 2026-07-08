// Typed wrappers over the Tauri command bridge. Every function maps 1:1 to a
// `#[tauri::command]` in src-tauri/src/commands.rs. Errors reject with a
// `CmdError` object ({ kind, message }).
import { invoke } from "@tauri-apps/api/core";
import type {
  CustomGameSpec,
  DaemonStatus,
  Game,
  ResolveChoice,
  Root,
  RootSpec,
  SyncedConfig,
  Uuid,
  Version,
} from "./types";

export const listGames = () => invoke<Game[]>("list_games");

export const listRoots = () => invoke<Root[]>("list_roots");

export const addRoot = (spec: RootSpec) => invoke<void>("add_root", { spec });

export const removeRoot = (id: Uuid) => invoke<void>("remove_root", { id });

export const backupNow = (gameId: Uuid) =>
  invoke<void>("backup_now", { gameId });

export const listVersions = (gameId: Uuid) =>
  invoke<Version[]>("list_versions", { gameId });

export const restore = (gameId: Uuid, versionId: Uuid) =>
  invoke<void>("restore", { gameId, versionId });

export const resolveConflict = (gameId: Uuid, choice: ResolveChoice) =>
  invoke<void>("resolve_conflict", { gameId, choice });

export const getStatus = () => invoke<DaemonStatus>("get_status");

export const setAutostart = (enabled: boolean) =>
  invoke<void>("set_autostart", { enabled });

export const getConfig = () => invoke<SyncedConfig>("get_config");

export const updateConfig = (config: SyncedConfig) =>
  invoke<void>("update_config", { config });

export const enterLearnMode = (gameId: Uuid) =>
  invoke<void>("enter_learn_mode", { gameId });

export const pairDevice = (
  serverUrl: string,
  code: string,
  deviceName: string,
) => invoke<Uuid>("pair_device", { serverUrl, code, deviceName });

export const addCustomGame = (spec: CustomGameSpec) =>
  invoke<void>("add_custom_game", { spec });

export const removeCustomGame = (title: string) =>
  invoke<void>("remove_custom_game", { title });
