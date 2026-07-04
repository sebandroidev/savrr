<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { getStatus } from "../lib/api";
  import type { DaemonStatus } from "../lib/types";
  import { errorMessage, isDaemonDown } from "../lib/types";
  import { formatBytes, formatUptime, formatRelative } from "../lib/format";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";

  let status = $state<DaemonStatus | null>(null);
  let error = $state<string | null>(null);
  let daemonDown = $state(false);
  let loading = $state(true);
  let timer: ReturnType<typeof setInterval> | undefined;

  async function refresh() {
    try {
      status = await getStatus();
      error = null;
      daemonDown = false;
    } catch (e) {
      error = errorMessage(e);
      daemonDown = isDaemonDown(e);
      status = null;
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    refresh();
    timer = setInterval(refresh, 3000);
  });
  onDestroy(() => timer && clearInterval(timer));

  const ramMb = $derived(status ? status.rss_bytes / (1024 * 1024) : 0);
  // The footprint story (PRD-07 §6): well under 64 MB is the target.
  const ramTone = $derived(ramMb <= 64 ? "good" : ramMb <= 120 ? "warn" : "bad");
</script>

<div class="head">
  <div>
    <h1>Dashboard</h1>
    <p class="muted">Live daemon health — the always-on engine behind Savr.</p>
  </div>
  <button class="ghost sm" onclick={refresh} disabled={loading}>
    <Icon name="refresh" size={15} /> Refresh
  </button>
</div>

{#if loading && !status}
  <div class="card center"><Spinner size={22} /></div>
{:else if daemonDown}
  <div class="card offline">
    <div class="badge bad"><span class="dot bad"></span> Daemon offline</div>
    <p class="muted">
      The Savr daemon isn't reachable. It runs in the background and does the
      actual detection and backups — start it (or the Savr service) and this
      view will come alive.
    </p>
    <code class="mono dim">{error}</code>
  </div>
{:else if error}
  <div class="card offline">
    <div class="badge warn">Couldn't read status</div>
    <code class="mono dim">{error}</code>
  </div>
{:else if status}
  <div class="grid">
    <div class="stat">
      <div class="stat-top">
        <span class="stat-ic"><Icon name="cpu" size={16} /></span>
        <span class="stat-label">Memory</span>
      </div>
      <div class="stat-value">
        {formatBytes(status.rss_bytes)}
        <span class="badge {ramTone}">{ramTone === "good" ? "tiny" : ramTone === "warn" ? "watch" : "high"}</span>
      </div>
      <div class="stat-sub dim">resident set — target &lt; 64 MB</div>
    </div>

    <div class="stat">
      <div class="stat-top">
        <span class="stat-ic"><Icon name="clock" size={16} /></span>
        <span class="stat-label">Uptime</span>
      </div>
      <div class="stat-value">{formatUptime(status.uptime_s)}</div>
      <div class="stat-sub dim">daemon v{status.version}</div>
    </div>

    <div class="stat">
      <div class="stat-top">
        <span class="stat-ic"><Icon name="games" size={16} /></span>
        <span class="stat-label">Watched games</span>
      </div>
      <div class="stat-value">{status.watched_games}</div>
      <div class="stat-sub dim">under active detection</div>
    </div>

    <div class="stat">
      <div class="stat-top">
        <span class="stat-ic"><Icon name="server" size={16} /></span>
        <span class="stat-label">Server</span>
      </div>
      <div class="stat-value row">
        <span class="dot {status.server_connected ? 'good' : 'bad'}"></span>
        {status.server_connected ? "Connected" : "Offline"}
      </div>
      <div class="stat-sub dim">
        {status.pending_outbox} change{status.pending_outbox === 1 ? "" : "s"} pending sync
      </div>
    </div>
  </div>

  <div class="card lastrow">
    <div>
      <span class="stat-label">Last backup</span>
      <div class="lastval">{formatRelative(status.last_backup_at)}</div>
    </div>
    <div class="badge accent">
      <Icon name="box" size={14} /> {status.pending_outbox} in outbox
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
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 14px;
  }
  .stat {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px;
  }
  .stat-top {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-muted);
    margin-bottom: 12px;
  }
  .stat-ic {
    display: flex;
    color: var(--accent);
  }
  .stat-label {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .stat-value {
    font-size: 26px;
    font-weight: 680;
    letter-spacing: -0.02em;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .stat-value.row {
    font-size: 20px;
  }
  .stat-sub {
    margin-top: 6px;
    font-size: 12px;
  }
  .lastrow {
    margin-top: 14px;
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .lastval {
    font-size: 18px;
    font-weight: 620;
    margin-top: 4px;
  }
  .center {
    display: flex;
    justify-content: center;
    padding: 48px;
    color: var(--text-muted);
  }
  .offline {
    display: flex;
    flex-direction: column;
    gap: 12px;
    align-items: flex-start;
  }
  .offline p {
    margin: 0;
    max-width: 60ch;
  }
</style>
