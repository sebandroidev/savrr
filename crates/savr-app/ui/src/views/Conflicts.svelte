<script lang="ts">
  import { onMount } from "svelte";
  import { listGames, resolveConflict } from "../lib/api";
  import type { Game, ResolveChoice } from "../lib/types";
  import { errorMessage, isDaemonDown } from "../lib/types";
  import { notify } from "../lib/toasts";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";

  // NOTE: live conflict push (daemon -> GUI ConflictRaised events) needs a
  // long-lived event stream that is a follow-up milestone. Until then this
  // view is a manual resolver: pick the game the daemon flagged and choose a
  // resolution. The three GuiRequest::ResolveConflict choices are wired.
  let games = $state<Game[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let daemonDown = $state(false);
  let selectedId = $state<string>("");
  let busy = $state<ResolveChoice | null>(null);

  const options: { choice: ResolveChoice; title: string; desc: string; icon: string }[] = [
    { choice: "keep_mine", title: "Keep mine", desc: "This device's save wins. The other side is superseded.", icon: "cpu" },
    { choice: "keep_theirs", title: "Keep theirs", desc: "The incoming remote save wins and is pulled down.", icon: "server" },
    { choice: "keep_both", title: "Keep both", desc: "Fork — retain both saves as separate branches, resolve later.", icon: "box" },
  ];

  async function load() {
    loading = true;
    try {
      games = await listGames();
      error = null;
      daemonDown = false;
      if (!selectedId && games.length) selectedId = games[0].id;
    } catch (e) {
      error = errorMessage(e);
      daemonDown = isDaemonDown(e);
    } finally {
      loading = false;
    }
  }

  async function resolve(choice: ResolveChoice) {
    if (!selectedId) return;
    busy = choice;
    try {
      await resolveConflict(selectedId, choice);
      const game = games.find((g) => g.id === selectedId);
      notify.success(`Resolved ${game?.title ?? "conflict"} — ${choice.replace("_", " ")}.`);
    } catch (e) {
      notify.error(errorMessage(e));
    } finally {
      busy = null;
    }
  }

  onMount(load);
</script>

<div class="head">
  <div>
    <h1>Conflicts</h1>
    <p class="muted">
      When the same save changes in two places, you decide which wins.
    </p>
  </div>
  <button class="ghost sm" onclick={load} disabled={loading}>
    <Icon name="refresh" size={15} /> Refresh
  </button>
</div>

{#if loading && games.length === 0}
  <div class="card center"><Spinner size={22} /></div>
{:else if daemonDown}
  <div class="card offline">
    <div class="badge bad"><span class="dot bad"></span> Daemon offline</div>
    <p class="muted">Start the Savr daemon to resolve conflicts.</p>
  </div>
{:else if error}
  <div class="card offline"><div class="badge warn">Error</div><code class="dim mono">{error}</code></div>
{:else}
  <div class="card">
    <div class="note">
      <Icon name="conflicts" size={16} />
      <span class="muted">
        Savr flags a conflict automatically when it detects divergent saves.
        Pick the affected game below and choose how to resolve it.
      </span>
    </div>

    <label for="game">Game</label>
    <select id="game" bind:value={selectedId}>
      {#if games.length === 0}
        <option value="">No games available</option>
      {/if}
      {#each games as g (g.id)}
        <option value={g.id}>{g.title}</option>
      {/each}
    </select>

    <div class="options">
      {#each options as opt}
        <button
          class="opt"
          onclick={() => resolve(opt.choice)}
          disabled={!selectedId || busy !== null}
        >
          <span class="opt-ic">
            {#if busy === opt.choice}<Spinner size={18} />{:else}<Icon name={opt.icon} size={18} />{/if}
          </span>
          <span class="opt-title">{opt.title}</span>
          <span class="opt-desc dim">{opt.desc}</span>
        </button>
      {/each}
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
  .note {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    padding: 12px;
    background: var(--warn-soft);
    border-radius: var(--radius-sm);
    margin-bottom: 18px;
    color: var(--warn);
  }
  .note span {
    font-size: 13px;
  }
  .options {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
    gap: 12px;
    margin-top: 18px;
  }
  .opt {
    display: grid;
    grid-template-columns: auto 1fr;
    grid-template-rows: auto auto;
    gap: 4px 12px;
    text-align: left;
    padding: 16px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .opt:hover:not(:disabled) {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .opt-ic {
    grid-row: 1 / 3;
    display: flex;
    align-items: flex-start;
    color: var(--accent);
    padding-top: 2px;
  }
  .opt-title {
    font-weight: 620;
    font-size: 14px;
  }
  .opt-desc {
    font-size: 12px;
    line-height: 1.45;
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
