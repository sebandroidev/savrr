<script lang="ts">
  import { toasts, dismissToast } from "../lib/toasts";
  import Icon from "./Icon.svelte";

  const icon: Record<string, string> = {
    info: "server",
    success: "check",
    error: "conflicts",
  };
</script>

<div class="host">
  {#each $toasts as t (t.id)}
    <div class="toast {t.kind}" role="status">
      <span class="ic"><Icon name={icon[t.kind]} size={16} /></span>
      <span class="msg">{t.message}</span>
      <button class="close" onclick={() => dismissToast(t.id)} aria-label="Dismiss">
        <Icon name="x" size={14} />
      </button>
    </div>
  {/each}
</div>

<style>
  .host {
    position: fixed;
    right: 18px;
    bottom: 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    z-index: 60;
    max-width: 380px;
  }
  .toast {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 11px 12px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-sm);
    box-shadow: var(--shadow);
    animation: slide 0.18s ease;
  }
  .toast.success {
    border-left-color: var(--good);
  }
  .toast.error {
    border-left-color: var(--bad);
  }
  .ic {
    display: flex;
    color: var(--accent);
  }
  .toast.success .ic {
    color: var(--good);
  }
  .toast.error .ic {
    color: var(--bad);
  }
  .msg {
    flex: 1;
    font-size: 13px;
  }
  .close {
    padding: 3px;
    border: none;
    background: transparent;
    color: var(--text-dim);
  }
  .close:hover {
    color: var(--text);
    background: transparent;
  }
  @keyframes slide {
    from {
      transform: translateY(6px);
      opacity: 0;
    }
  }
</style>
