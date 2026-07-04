// Update flow (PRD-04 / PRD-07 §5). On launch we auto-check silently; the
// Settings view also exposes an explicit "Check for updates" action. Every
// install is gated behind a native confirm dialog so we never restart the app
// from under the user.
import { check } from "@tauri-apps/plugin-updater";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";

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
      `Version ${update.version} is available.\n\nDownload and install it now?`,
      { title: "Update available", kind: "info", okLabel: "Install", cancelLabel: "Later" },
    );
    if (!proceed) return { status: "declined", version: update.version };

    await update.downloadAndInstall();

    const restartNow = await ask(
      "Update installed. Restart Savr now to finish?",
      { title: "Restart Savr", kind: "info", okLabel: "Restart", cancelLabel: "Not now" },
    );
    if (restartNow) {
      await relaunch();
    }
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
