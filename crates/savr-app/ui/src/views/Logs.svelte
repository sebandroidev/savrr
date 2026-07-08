<script lang="ts">
  import { onMount } from "svelte";
  import { save } from "@tauri-apps/plugin-dialog";
  import { getLogs, writeTextFile } from "../lib/api";
  import { appLog, clearAppLog } from "../lib/devlog";
  import { notify } from "../lib/toasts";
  import { errorMessage } from "../lib/types";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";

  let daemonLines = $state<string[]>([]);
  let loading = $state(false);
  let daemonError = $state<string | null>(null);

  async function loadDaemonLogs() {
    loading = true;
    daemonError = null;
    try {
      daemonLines = await getLogs(1000);
    } catch (e) {
      daemonError = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  onMount(loadDaemonLogs);

  // A single plain-text bundle of both panes, for copy/download.
  function bundle(): string {
    const app = $appLog.map((e) => `${e.ts} [${e.level}] ${e.message}`).join("\n");
    return [
      "=== Savr app errors ===",
      app || "(none)",
      "",
      "=== Savr daemon log ===",
      daemonError ? `(daemon unreachable: ${daemonError})` : daemonLines.join("\n"),
      "",
    ].join("\n");
  }

  async function copyAll() {
    try {
      await navigator.clipboard.writeText(bundle());
      notify.success("Logs copied to clipboard.");
    } catch (e) {
      notify.error(errorMessage(e));
    }
  }

  async function downloadAll() {
    try {
      const path = await save({
        title: "Save Savr logs",
        defaultPath: "savr-logs.txt",
        filters: [{ name: "Text", extensions: ["txt"] }],
      });
      if (typeof path !== "string") return;
      await writeTextFile(path, bundle());
      notify.success("Logs saved.");
    } catch (e) {
      notify.error(errorMessage(e));
    }
  }
</script>

<section class="view">
  <header class="view-head">
    <div>
      <h1>Logs</h1>
      <p class="muted">Daemon activity and in-app errors, for diagnosing issues.</p>
    </div>
    <div class="actions">
      <button class="sm ghost" onclick={copyAll}><Icon name="copy" /> Copy</button>
      <button class="sm ghost" onclick={downloadAll}><Icon name="download" /> Download</button>
    </div>
  </header>

  <div class="panel">
    <div class="panel-head">
      <h2>App errors <span class="count">{$appLog.length}</span></h2>
      <button class="sm ghost" onclick={clearAppLog} disabled={$appLog.length === 0}>Clear</button>
    </div>
    {#if $appLog.length === 0}
      <p class="empty">No app errors captured this session.</p>
    {:else}
      <pre class="log">{#each $appLog as e (e.ts + e.message)}<span class="line {e.level}">{e.ts}  {e.message}</span>
{/each}</pre>
    {/if}
  </div>

  <div class="panel">
    <div class="panel-head">
      <h2>Daemon log</h2>
      <button class="sm ghost" onclick={loadDaemonLogs} disabled={loading}>
        {#if loading}<Spinner />{/if} Refresh
      </button>
    </div>
    {#if daemonError}
      <p class="empty error">Couldn't reach the daemon: {daemonError}</p>
    {:else if daemonLines.length === 0}
      <p class="empty">No daemon log lines.</p>
    {:else}
      <pre class="log">{daemonLines.join("\n")}</pre>
    {/if}
  </div>
</section>

<style>
  .view-head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 1rem;
    margin-bottom: 1rem;
  }
  .actions {
    display: flex;
    gap: 0.4rem;
  }
  .panel {
    border: 1px solid var(--border);
    border-radius: 8px;
    margin-bottom: 1rem;
    overflow: hidden;
  }
  .panel-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid var(--border);
  }
  .panel-head h2 {
    font-size: 0.85rem;
    margin: 0;
  }
  .count {
    font-weight: 400;
    color: var(--muted);
  }
  .log {
    margin: 0;
    padding: 0.75rem;
    max-height: 40vh;
    overflow: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.75rem;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .line {
    display: block;
  }
  .line.error {
    color: var(--danger, #e5534b);
  }
  .empty {
    padding: 0.75rem;
    color: var(--muted);
    margin: 0;
  }
  .empty.error {
    color: var(--danger, #e5534b);
  }
</style>
