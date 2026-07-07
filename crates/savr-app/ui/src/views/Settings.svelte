<script lang="ts">
  import { onMount } from "svelte";
  import {
    getConfig,
    updateConfig,
    getStatus,
    setAutostart,
  } from "../lib/api";
  import type {
    SyncedConfig,
    ConflictPolicy,
    AutoPullPolicy,
  } from "../lib/types";
  import { errorMessage, isDaemonDown } from "../lib/types";
  import { notify } from "../lib/toasts";
  import { checkForUpdates } from "../lib/updater";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";

  interface Props {
    theme: "dark" | "light";
    onToggleTheme: () => void;
  }
  let { theme, onToggleTheme }: Props = $props();

  const CONFLICT: { value: ConflictPolicy; label: string }[] = [
    { value: "manual", label: "Manual — ask me every time" },
    { value: "latest_wins", label: "Latest wins — newest mtime" },
    { value: "theirs_wins", label: "Theirs wins — prefer remote" },
    { value: "mine_wins", label: "Mine wins — prefer this device" },
  ];
  const AUTOPULL: { value: AutoPullPolicy; label: string }[] = [
    { value: "ask", label: "Ask before pulling remote saves" },
    { value: "auto", label: "Auto — pull without asking" },
  ];

  let config = $state<SyncedConfig | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let daemonDown = $state(false);
  let saving = $state(false);
  let checking = $state(false);
  let autostart = $state(false);
  let autostartBusy = $state(false);

  async function load() {
    loading = true;
    try {
      config = await getConfig();
      autostart = (await getStatus()).autostart_enabled;
      error = null;
      daemonDown = false;
    } catch (e) {
      error = errorMessage(e);
      daemonDown = isDaemonDown(e);
    } finally {
      loading = false;
    }
  }

  async function save() {
    if (!config) return;
    saving = true;
    try {
      await updateConfig(config);
      notify.success("Settings saved.");
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      saving = false;
    }
  }

  async function checkUpdates() {
    checking = true;
    try {
      const r = await checkForUpdates({ silent: false });
      if (r.status === "up-to-date") notify.info("Savr is up to date.");
      else if (r.status === "installed") notify.success(`Installed v${r.version}.`);
    } finally {
      checking = false;
    }
  }

  async function toggleAutostart() {
    autostartBusy = true;
    const next = !autostart;
    try {
      await setAutostart(next);
      autostart = next;
      notify.success(
        next
          ? "Savr will start in the background when you sign in to Windows."
          : "Savr will no longer start on sign-in.",
      );
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      autostartBusy = false;
    }
  }

  onMount(load);
</script>

<div class="head">
  <div>
    <h1>Settings</h1>
    <p class="muted">Sync policies and app preferences.</p>
  </div>
  <button class="ghost sm" onclick={onToggleTheme} title="Toggle theme">
    <Icon name={theme === "dark" ? "sun" : "moon"} size={15} />
    {theme === "dark" ? "Light" : "Dark"}
  </button>
</div>

{#if loading && !config}
  <div class="card center"><Spinner size={22} /></div>
{:else if daemonDown}
  <div class="card offline">
    <div class="badge bad"><span class="dot bad"></span> Daemon offline</div>
    <p class="muted">Start the Savr daemon to view and change sync policies.</p>
  </div>
{:else if error}
  <div class="card offline"><div class="badge warn">Error</div><code class="dim mono">{error}</code></div>
{:else if config}
  <div class="card section">
    <h2>Sync policies</h2>
    <div class="field">
      <label for="conflict">Conflict resolution</label>
      <select id="conflict" bind:value={config.conflict_policy}>
        {#each CONFLICT as c}<option value={c.value}>{c.label}</option>{/each}
      </select>
    </div>
    <div class="field">
      <label for="autopull">Auto-pull</label>
      <select id="autopull" bind:value={config.autopull_policy}>
        {#each AUTOPULL as a}<option value={a.value}>{a.label}</option>{/each}
      </select>
    </div>

    <h3 class="sub">Retention</h3>
    <div class="two">
      <div class="field">
        <label for="full">Full snapshots kept</label>
        <input id="full" type="number" min="1" bind:value={config.retention.full} />
      </div>
      <div class="field">
        <label for="diff">Diffs per full</label>
        <input id="diff" type="number" min="1" bind:value={config.retention.diff_per_full} />
      </div>
    </div>

    <div class="save-row">
      <span class="dim mono">config tag: {config.tag || "—"}</span>
      <button class="primary" onclick={save} disabled={saving}>
        {#if saving}<Spinner size={14} />{:else}<Icon name="check" size={15} />{/if}
        Save changes
      </button>
    </div>
  </div>

  <div class="card section">
    <h2>Application</h2>
    <div class="app-row">
      <div>
        <div class="app-title">Start on Windows sign-in</div>
        <div class="dim">
          Runs Savr in the background at login — no window — so games are
          detected and saves backed up even in Xbox Full Screen mode.
        </div>
      </div>
      <button
        onclick={toggleAutostart}
        disabled={autostartBusy}
        class:primary={!autostart}
      >
        {#if autostartBusy}<Spinner size={14} />{:else}<Icon
            name={autostart ? "check" : "play"}
            size={15}
          />{/if}
        {autostart ? "Turn off" : "Turn on"}
      </button>
    </div>
    <div class="app-row">
      <div>
        <div class="app-title">Software updates</div>
        <div class="dim">Auto-checked on launch. Installs are always confirmed.</div>
      </div>
      <button onclick={checkUpdates} disabled={checking}>
        {#if checking}<Spinner size={14} />{:else}<Icon name="download" size={15} />{/if}
        Check for updates
      </button>
    </div>
    <div class="app-row">
      <div>
        <div class="app-title">Theme</div>
        <div class="dim">Savr ships dark; switch anytime.</div>
      </div>
      <button onclick={onToggleTheme}>
        <Icon name={theme === "dark" ? "sun" : "moon"} size={15} />
        {theme === "dark" ? "Light mode" : "Dark mode"}
      </button>
    </div>
  </div>
{/if}

<style>
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    margin-bottom: 20px;
    gap: 12px;
  }
  .head p {
    margin: 4px 0 0;
    font-size: 13px;
  }
  .section {
    margin-bottom: 16px;
  }
  .section h2 {
    font-size: 15px;
    margin-bottom: 16px;
  }
  .sub {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
    margin: 20px 0 12px;
  }
  .field {
    margin-bottom: 14px;
    max-width: 460px;
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 14px;
    max-width: 460px;
  }
  .save-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-top: 20px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
  }
  .app-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
    padding: 12px 0;
  }
  .app-row + .app-row {
    border-top: 1px solid var(--border);
  }
  .app-title {
    font-weight: 580;
    font-size: 13.5px;
  }
  .dim {
    font-size: 12.5px;
  }
  .center {
    display: flex;
    justify-content: center;
    padding: 40px;
    color: var(--text-muted);
  }
  .offline {
    display: flex;
    flex-direction: column;
    gap: 10px;
    align-items: flex-start;
  }
  .offline p {
    margin: 0;
  }
</style>
