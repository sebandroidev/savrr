// In-app log buffer for the Developer view. Captures webview errors, unhandled
// promise rejections, and every error toast — the things that never reach the
// daemon's log file, so they'd otherwise be invisible (this is exactly what hid
// the silent "add game folder" failure).
import { writable } from "svelte/store";

export type AppLogLevel = "error" | "warn" | "info";

export interface AppLogEntry {
  ts: string; // ISO timestamp
  level: AppLogLevel;
  message: string;
}

const MAX_ENTRIES = 500;

export const appLog = writable<AppLogEntry[]>([]);

export function logEvent(level: AppLogLevel, message: string) {
  const entry: AppLogEntry = { ts: new Date().toISOString(), level, message };
  appLog.update((list) => {
    const next = [...list, entry];
    return next.length > MAX_ENTRIES ? next.slice(next.length - MAX_ENTRIES) : next;
  });
}

export function clearAppLog() {
  appLog.set([]);
}

let installed = false;

/// Wire the global webview error hooks once. Safe to call more than once.
export function installGlobalErrorCapture() {
  if (installed) return;
  installed = true;
  window.addEventListener("error", (e) => {
    const where = e.filename ? ` (${e.filename}:${e.lineno}:${e.colno})` : "";
    logEvent("error", `${e.message}${where}`);
  });
  window.addEventListener("unhandledrejection", (e) => {
    const reason = e.reason instanceof Error ? e.reason.message : String(e.reason);
    logEvent("error", `Unhandled rejection: ${reason}`);
  });
}
