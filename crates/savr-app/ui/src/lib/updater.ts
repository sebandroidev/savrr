// Update flow (PRD-04 / PRD-07 §5). On launch we auto-check silently; the
// Settings view also exposes an explicit "Check for updates" action. Every
// install is gated behind a native confirm dialog so we never restart the app
// from under the user.
import { check } from "@tauri-apps/plugin-updater";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";

export interface UpdateOutcome {
  status: "up-to-date" | "declined" | "installed" | "error";
  version?: string;
  detail?: string;
}

export async function checkForUpdates(
  opts: { silent?: boolean } = {},
): Promise<UpdateOutcome> {
  const silent = opts.silent ?? true;
  try {
    const update = await check();
    if (!update) {
      if (!silent) {
        await message("Savr is up to date.", { title: "Savr", kind: "info" });
      }
      return { status: "up-to-date" };
    }

    const proceed = await ask(
      `Version ${update.version} is available.\n\nSavr will install it and restart to finish. Continue?`,
      { title: "Update available", kind: "info", okLabel: "Update & restart", cancelLabel: "Later" },
    );
    if (!proceed) return { status: "declined", version: update.version };

    await update.downloadAndInstall();

    // Full clean teardown (app + daemon) then relaunch, so no stale process
    // survives the update and serves old code. This never returns — the app
    // exits and a fresh instance takes over.
    await invoke("restart_for_update");
    return { status: "installed", version: update.version };
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    if (!silent) {
      await message(`Update check failed: ${detail}`, {
        title: "Savr",
        kind: "error",
      });
    }
    console.error("update check failed", e);
    return { status: "error", detail };
  }
}
