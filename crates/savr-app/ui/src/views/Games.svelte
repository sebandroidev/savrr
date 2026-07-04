<script lang="ts">
  import { onMount } from "svelte";
  import {
    listGames,
    listVersions,
    backupNow,
    restore,
    enterLearnMode,
  } from "../lib/api";
  import type { Game, Version } from "../lib/types";
  import { errorMessage, isDaemonDown } from "../lib/types";
  import { formatBytes, formatDateTime, shortId } from "../lib/format";
  import { notify } from "../lib/toasts";
  import { confirm } from "@tauri-apps/plugin-dialog";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";

  let games = $state<Game[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let daemonDown = $state(false);

  let selected = $state<Game | null>(null);
  let versions = $state<Version[]>([]);
  let versionsLoading = $state(false);
  let versionsError = $state<string | null>(null);
  let busyId = $state<string | null>(null);

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

  onMount(loadGames);
</script>

<div class="head">
  <div>
    <h1>Games</h1>
    <p class="muted">Everything Savr is watching, with full version history.</p>
  </div>
  <button class="ghost sm" onclick={loadGames} disabled={loading}>
    <Icon name="refresh" size={15} /> Refresh
  </button>
</div>

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
          <button
            class="row-btn"
            class:active={selected?.id === game.id}
            onclick={() => select(game)}
          >
            <span class="title">{game.title}</span>
            <span class="meta">
              <span class="badge">{game.source}</span>
              {#if game.steam_appid}<span class="dim mono">#{game.steam_appid}</span>{/if}
            </span>
          </button>
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
    width: 100%;
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
