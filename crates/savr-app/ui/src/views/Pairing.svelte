<script lang="ts">
  import { pairDevice } from "../lib/api";
  import { errorMessage } from "../lib/types";
  import { notify } from "../lib/toasts";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";

  interface Props {
    onPaired: () => void;
    onSkip: () => void;
  }
  let { onPaired, onSkip }: Props = $props();

  let serverUrl = $state("");
  let code = $state("");
  let deviceName = $state(defaultDeviceName());
  let pairing = $state(false);
  let error = $state<string | null>(null);

  function defaultDeviceName(): string {
    const p = navigator.platform || "";
    if (/win/i.test(p)) return "My Windows PC";
    if (/mac/i.test(p)) return "My Mac";
    if (/linux/i.test(p)) return "My Linux PC";
    return "My device";
  }

  const canSubmit = $derived(
    serverUrl.trim().length > 0 &&
      code.trim().length > 0 &&
      deviceName.trim().length > 0 &&
      !pairing,
  );

  async function submit() {
    error = null;
    pairing = true;
    try {
      await pairDevice(serverUrl.trim(), code.trim(), deviceName.trim());
      notify.success("Device paired.");
      onPaired();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      pairing = false;
    }
  }
</script>

<div class="wrap">
  <div class="wizard">
    <div class="brand">
      <span class="logo"><Icon name="box" size={22} /></span>
      <div>
        <h1>Welcome to Savr</h1>
        <p class="muted">Pair this device with your Savr server to start syncing saves.</p>
      </div>
    </div>

    <div class="steps">
      <span class="step"><span class="n">1</span> Point at your server</span>
      <span class="step"><span class="n">2</span> Enter your pairing code</span>
      <span class="step"><span class="n">3</span> Name this device</span>
    </div>

    <form onsubmit={(e) => { e.preventDefault(); if (canSubmit) submit(); }}>
      <div class="field">
        <label for="url">Server URL</label>
        <input
          id="url"
          placeholder="https://savr.mynas.local:8080"
          bind:value={serverUrl}
          autocomplete="off"
        />
      </div>
      <div class="field">
        <label for="code">Pairing code</label>
        <input id="code" placeholder="e.g. 7F3K-92QD" bind:value={code} autocomplete="off" />
      </div>
      <div class="field">
        <label for="name">Device name</label>
        <input id="name" bind:value={deviceName} autocomplete="off" />
      </div>

      {#if error}
        <div class="err">
          <Icon name="conflicts" size={15} />
          <span>{error}</span>
        </div>
      {/if}

      <div class="actions">
        <button type="button" class="ghost" onclick={onSkip} disabled={pairing}>
          Skip for now
        </button>
        <button type="submit" class="primary" disabled={!canSubmit}>
          {#if pairing}<Spinner size={14} />{:else}<Icon name="link" size={15} />{/if}
          Pair device
        </button>
      </div>
    </form>
  </div>
</div>

<style>
  .wrap {
    height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    background:
      radial-gradient(1200px 600px at 50% -10%, var(--accent-soft), transparent 60%),
      var(--bg);
  }
  .wizard {
    width: 100%;
    max-width: 440px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 14px;
    box-shadow: var(--shadow);
    padding: 26px;
  }
  .brand {
    display: flex;
    gap: 14px;
    align-items: center;
    margin-bottom: 22px;
  }
  .logo {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    border-radius: 12px;
    background: var(--accent-soft);
    color: var(--accent);
    flex-shrink: 0;
  }
  .brand h1 {
    font-size: 20px;
  }
  .brand p {
    margin: 3px 0 0;
    font-size: 13px;
  }
  .steps {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px 16px;
    background: var(--panel-2);
    border-radius: var(--radius-sm);
    margin-bottom: 22px;
  }
  .step {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13px;
    color: var(--text-muted);
  }
  .n {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--accent);
    color: #fff;
    font-size: 11px;
    font-weight: 700;
  }
  .field {
    margin-bottom: 14px;
  }
  .err {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 10px 12px;
    background: var(--bad-soft);
    color: var(--bad);
    border-radius: var(--radius-sm);
    font-size: 13px;
    margin-bottom: 14px;
  }
  .actions {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    margin-top: 20px;
  }
</style>
