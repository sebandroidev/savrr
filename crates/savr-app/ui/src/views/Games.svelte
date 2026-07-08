<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import {
    listGames,
    listVersions,
    backupNow,
    restore,
    enterLearnMode,
    addRoot,
    removeCustomGame,
  } from "../lib/api";
  import type { Game, Version } from "../lib/types";
  import { errorMessage, isDaemonDown } from "../lib/types";
  import {
    formatBytes,
    formatDateTime,
    formatRelative,
    formatUptime,
    shortId,
  } from "../lib/format";
  import { notify } from "../lib/toasts";
  import { confirm, open } from "@tauri-apps/plugin-dialog";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";
  import AddGameDialog from "./AddGameDialog.svelte";

  let games = $state<Game[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let daemonDown = $state(false);

  let selected = $state<Game | null>(null);
  let versions = $state<Version[]>([]);
  let versionsLoading = $state(false);
  let versionsError = $state<string | null>(null);
  let busyId = $state<string | null>(null);

  let addingFolder = $state(false);
  let showAddGame = $state(false);

  async function loadGames() {
    loading = true;
    try {
      games = await listGames();
      error = null;
      daemonDown = false;
      if (selected) {
        selected = games.find((g) => g.id === selected!.id) ?? null;
      }
    } catch (e) {
      error = errorMessage(e);
      daemonDown = isDaemonDown(e);
    } finally {
      loading = false;
    }
  }

  async function select(game: Game) {
    selected = game;
    versions = [];
    versionsError = null;
    versionsLoading = true;
    try {
      versions = await listVersions(game.id);
    } catch (e) {
      versionsError = errorMessage(e);
    } finally {
      versionsLoading = false;
    }
  }

  async function doBackup(game: Game) {
    busyId = game.id;
    try {
      await backupNow(game.id);
      // A game with no known save paths captures nothing (the daemon returns
      // NoChange), so don't claim a backup was queued — tell the user how to
      // teach Savrr where it saves.
      if (game.save_targets.length === 0) {
        notify.info(
          `Savr doesn't know where ${game.title} saves yet — turn on Learn mode and play it once.`,
        );
      } else {
        notify.success(`Backup queued for ${game.title}.`);
      }
      if (selected?.id === game.id) await select(game);
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      busyId = null;
    }
  }

  async function doLearn(game: Game) {
    busyId = game.id;
    try {
      await enterLearnMode(game.id);
      notify.info(`Learn mode on for ${game.title} — launch it to capture its exe.`);
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      busyId = null;
    }
  }

  async function doRestore(game: Game, v: Version) {
    const ok = await confirm(
      `Restore ${game.title} to the snapshot from ${formatDateTime(v.created_at)}?\n\nThis overwrites the current save on disk.`,
      { title: "Restore save", kind: "warning", okLabel: "Restore" },
    );
    if (!ok) return;
    busyId = v.id;
    try {
      await restore(game.id, v.id);
      notify.success(`${game.title} restored to seq #${v.seq}.`);
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      busyId = null;
    }
  }

  async function addGameFolder() {
    const dir = await open({
      directory: true,
      title: "Pick a folder that contains your games",
    });
    if (typeof dir !== "string") return;
    addingFolder = true;
    try {
      await addRoot({ kind: "drive", path: dir });
      notify.success("Game folder added.");
      await loadGames();
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      addingFolder = false;
    }
  }

  async function removeGame(game: Game) {
    const ok = await confirm(
      `Remove ${game.title} from Savr?\n\nThis won't delete any saves already backed up.`,
      { title: "Remove game", kind: "warning", okLabel: "Remove" },
    );
    if (!ok) return;
    busyId = game.id;
    try {
      await removeCustomGame(game.title);
      notify.success(`${game.title} removed.`);
      if (selected?.id === game.id) selected = null;
      await loadGames();
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      busyId = null;
    }
  }

  onMount(() => {
    loadGames();
    // The daemon builds its catalog after a slow startup manifest fetch, so the
    // first load can come back empty. It emits "catalog-updated" when the catalog
    // (re)builds — reload then instead of leaving a stale empty list.
    const unlisten = listen("catalog-updated", () => loadGames());
    return () => {
      unlisten.then((off) => off());
    };
  });
</script>

<div class="head">
  <div>
    <h1>Games</h1>
    <p class="muted">Everything Savr is watching, with full version history.</p>
  </div>
  <div class="head-actions">
    <button class="sm" onclick={addGameFolder} disabled={addingFolder}>
      {#if addingFolder}<Spinner size={14} />{:else}<Icon name="roots" size={15} />{/if}
      Add game folder
    </button>
    <button class="sm" onclick={() => (showAddGame = true)}>
      <Icon name="plus" size={15} /> Add game
    </button>
    <button class="ghost sm" onclick={loadGames} disabled={loading}>
      <Icon name="refresh" size={15} /> Refresh
    </button>
  </div>
</div>

{#if showAddGame}
  <AddGameDialog
    onSaved={() => {
      showAddGame = false;
      loadGames();
    }}
    onClose={() => (showAddGame = false)}
  />
{/if}

{#if loading && games.length === 0}
  <div class="card center"><Spinner size={22} /></div>
{:else if daemonDown}
  <div class="card offline">
    <div class="badge bad"><span class="dot bad"></span> Daemon offline</div>
    <p class="muted">Start the Savr daemon to see your watched games.</p>
  </div>
{:else if error}
  <div class="card offline"><div class="badge warn">Error</div><code class="dim mono">{error}</code></div>
{:else if games.length === 0}
  <div class="card center col">
    <Icon name="games" size={28} />
    <p class="muted">No games watched yet. Add a root, and Savr will match installed games.</p>
  </div>
{:else}
  <div class="split">
    <ul class="list">
      {#each games as game (game.id)}
        <li>
          <div class="row-wrap">
            <button
              class="row-btn"
              class:active={selected?.id === game.id}
              onclick={() => select(game)}
            >
              <span class="title">{game.title}</span>
              <span class="meta">
                <span class="badge">{game.source}</span>
                {#if game.steam_appid}<span class="dim mono">#{game.steam_appid}</span>{/if}
                {#if game.running}
                  <span class="badge live"><span class="pulse"></span>Playing</span>
                {:else if game.last_played}
                  <span class="dim">Played {formatRelative(game.last_played)}</span>
                {/if}
              </span>
            </button>
            {#if game.source === "Custom"}
              <button
                class="danger sm remove-btn"
                onclick={() => removeGame(game)}
                disabled={busyId === game.id}
                aria-label={`Remove ${game.title}`}
              >
                {#if busyId === game.id}<Spinner size={13} />{:else}<Icon name="trash" size={13} />{/if}
              </button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>

    <div class="detail">
      {#if !selected}
        <div class="card center col dim">
          <Icon name="games" size={24} />
          <span>Select a game to see its history.</span>
        </div>
      {:else}
        <div class="card">
          <div class="detail-head">
            <div>
              <h2>{selected.title}</h2>
              <span class="dim mono">{shortId(selected.id)}</span>
              <div class="playstat">
                {#if selected.running}
                  <span class="badge live"><span class="pulse"></span>Playing now</span>
                {/if}
                <span class="dim"
                  >Last played: {selected.last_played
                    ? formatDateTime(selected.last_played)
                    : "never"}</span
                >
                {#if selected.last_session_secs != null}
                  <span class="dim">· Last session {formatUptime(selected.last_session_secs)}</span>
                {/if}
                {#if selected.total_secs > 0}
                  <span class="dim">· {formatUptime(selected.total_secs)} total</span>
                {/if}
              </div>
            </div>
            <div class="actions">
              <button class="sm" onclick={() => doLearn(selected!)} disabled={busyId === selected.id}>
                <Icon name="play" size={14} /> Learn mode
              </button>
              <button class="primary sm" onclick={() => doBackup(selected!)} disabled={busyId === selected.id}>
                {#if busyId === selected.id}<Spinner size={14} />{:else}<Icon name="box" size={14} />{/if}
                Back up now
              </button>
            </div>
          </div>

          <div class="targets">
            {#each selected.save_targets as t}
              <span class="badge">{t.glob}</span>
            {/each}
          </div>

          <div class="history">
            <div class="history-head">
              <h3>Version history</h3>
              <span class="dim">{versions.length} snapshot{versions.length === 1 ? "" : "s"}</span>
            </div>

            {#if versionsLoading}
              <div class="center"><Spinner size={18} /></div>
            {:else if versionsError}
              <code class="dim mono">{versionsError}</code>
            {:else if versions.length === 0}
              <p class="dim">No snapshots yet. "Back up now" creates the first one.</p>
            {:else}
              <div class="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Seq</th><th>When</th><th>Kind</th><th>Size</th><th>Files</th><th></th>
                    </tr>
                  </thead>
                  <tbody>
                    {#each versions as v (v.id)}
                      <tr>
                        <td class="mono">#{v.seq}</td>
                        <td>{formatDateTime(v.created_at)}</td>
                        <td>
                          <span class="badge {v.kind === 'full' ? 'accent' : ''}">{v.kind}</span>
                        </td>
                        <td>{formatBytes(v.bytes)}</td>
                        <td class="dim">{v.files.length}</td>
                        <td class="right">
                          <button class="sm" onclick={() => doRestore(selected!, v)} disabled={busyId === v.id}>
                            {#if busyId === v.id}<Spinner size={13} />{:else}<Icon name="restore" size={13} />{/if}
                            Restore
                          </button>
                        </td>
                      </tr>
                    {/each}
                  </tbody>
                </table>
              </div>
            {/if}
          </div>
        </div>
      {/if}
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
  .head-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .row-wrap {
    display: flex;
    align-items: stretch;
    gap: 4px;
  }
  .remove-btn {
    flex-shrink: 0;
    align-self: center;
    padding: 6px 8px;
  }
  .split {
    display: grid;
    grid-template-columns: minmax(220px, 300px) 1fr;
    gap: 16px;
    align-items: start;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 6px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .row-btn {
    flex: 1;
    min-width: 0;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    padding: 10px 11px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row-btn:hover {
    background: var(--panel-2);
    border-color: var(--border);
  }
  .row-btn.active {
    background: var(--accent-soft);
    border-color: var(--accent);
  }
  .title {
    font-weight: 580;
    font-size: 13.5px;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .badge.live {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    background: color-mix(in srgb, var(--good, #2ecc71) 18%, transparent);
    color: var(--good, #2ecc71);
    border-color: transparent;
  }
  .pulse {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.35;
    }
  }
  .playstat {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
    font-size: 12.5px;
  }
  .detail-head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .targets {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 14px 0 4px;
  }
  .history {
    margin-top: 18px;
    border-top: 1px solid var(--border);
    padding-top: 16px;
  }
  .history-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 12px;
  }
  .table-wrap {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  th {
    text-align: left;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--text-dim);
    font-weight: 600;
    padding: 0 10px 8px;
  }
  td {
    padding: 9px 10px;
    border-top: 1px solid var(--border);
    white-space: nowrap;
  }
  td.right {
    text-align: right;
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
  h2 {
    font-size: 18px;
  }
  h3 {
    font-size: 14px;
  }
</style>
