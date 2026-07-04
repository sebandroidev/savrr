<script lang="ts">
  import { onMount } from "svelte";
  import { listRoots, addRoot, removeRoot } from "../lib/api";
  import type { Root, RootKind } from "../lib/types";
  import { errorMessage, isDaemonDown } from "../lib/types";
  import { notify } from "../lib/toasts";
  import { open, confirm } from "@tauri-apps/plugin-dialog";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";

  const KINDS: { value: RootKind; label: string }[] = [
    { value: "steam", label: "Steam library" },
    { value: "drive", label: "Drive / folder" },
    { value: "emulator", label: "Emulator" },
    { value: "launcher", label: "Launcher" },
  ];

  let roots = $state<Root[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let daemonDown = $state(false);

  let newKind = $state<RootKind>("steam");
  let newPath = $state("");
  let adding = $state(false);
  let removingId = $state<string | null>(null);

  async function load() {
    loading = true;
    try {
      roots = await listRoots();
      error = null;
      daemonDown = false;
    } catch (e) {
      error = errorMessage(e);
      daemonDown = isDaemonDown(e);
    } finally {
      loading = false;
    }
  }

  async function browse() {
    const picked = await open({ directory: true, multiple: false, title: "Choose a root folder" });
    if (typeof picked === "string") newPath = picked;
  }

  async function add() {
    const path = newPath.trim();
    if (!path) {
      notify.error("Pick a folder first.");
      return;
    }
    adding = true;
    try {
      await addRoot({ kind: newKind, path });
      notify.success("Root added.");
      newPath = "";
      await load();
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      adding = false;
    }
  }

  async function remove(root: Root) {
    const ok = await confirm(`Stop watching this ${root.kind} root?\n\n${root.path}`, {
      title: "Remove root",
      kind: "warning",
      okLabel: "Remove",
    });
    if (!ok) return;
    removingId = root.id;
    try {
      await removeRoot(root.id);
      notify.success("Root removed.");
      await load();
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      removingId = null;
    }
  }

  onMount(load);
</script>

<div class="head">
  <div>
    <h1>Roots</h1>
    <p class="muted">Folders Savr scans for installed games and save files.</p>
  </div>
  <button class="ghost sm" onclick={load} disabled={loading}>
    <Icon name="refresh" size={15} /> Refresh
  </button>
</div>

<div class="card add">
  <div class="field kind">
    <label for="kind">Kind</label>
    <select id="kind" bind:value={newKind}>
      {#each KINDS as k}
        <option value={k.value}>{k.label}</option>
      {/each}
    </select>
  </div>
  <div class="field path">
    <label for="path">Path</label>
    <div class="path-row">
      <input id="path" placeholder="/path/to/library" bind:value={newPath} />
      <button class="sm" onclick={browse} type="button">
        <Icon name="roots" size={14} /> Browse
      </button>
    </div>
  </div>
  <button class="primary add-btn" onclick={add} disabled={adding}>
    {#if adding}<Spinner size={14} />{:else}<Icon name="plus" size={15} />{/if}
    Add root
  </button>
</div>

{#if loading && roots.length === 0}
  <div class="card center"><Spinner size={22} /></div>
{:else if daemonDown}
  <div class="card offline">
    <div class="badge bad"><span class="dot bad"></span> Daemon offline</div>
    <p class="muted">Start the Savr daemon to manage roots.</p>
  </div>
{:else if error}
  <div class="card offline"><div class="badge warn">Error</div><code class="dim mono">{error}</code></div>
{:else if roots.length === 0}
  <div class="card center col">
    <Icon name="roots" size={26} />
    <p class="muted">No roots yet. Add your Steam library above to get started.</p>
  </div>
{:else}
  <ul class="list">
    {#each roots as root (root.id)}
      <li class="card row">
        <span class="r-ic"><Icon name="roots" size={18} /></span>
        <div class="r-main">
          <div class="r-path mono">{root.path}</div>
          <span class="badge accent">{root.kind}</span>
        </div>
        <button class="danger sm" onclick={() => remove(root)} disabled={removingId === root.id}>
          {#if removingId === root.id}<Spinner size={13} />{:else}<Icon name="trash" size={14} />{/if}
          Remove
        </button>
      </li>
    {/each}
  </ul>
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
  .add {
    display: grid;
    grid-template-columns: minmax(160px, 200px) 1fr auto;
    gap: 14px;
    align-items: end;
    margin-bottom: 16px;
  }
  .field label {
    margin-bottom: 6px;
  }
  .path-row {
    display: flex;
    gap: 8px;
  }
  .path-row input {
    flex: 1;
  }
  .path-row button {
    white-space: nowrap;
  }
  .add-btn {
    height: 37px;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 14px 16px;
  }
  .r-ic {
    display: flex;
    color: var(--accent);
  }
  .r-main {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }
  .r-path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
  }
  .center {
    display: flex;
    justify-content: center;
    padding: 40px;
    color: var(--text-muted);
  }
  .col {
    flex-direction: column;
    gap: 10px;
    text-align: center;
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
  @media (max-width: 720px) {
    .add {
      grid-template-columns: 1fr;
    }
  }
</style>
