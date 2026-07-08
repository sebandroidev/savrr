<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { addCustomGame } from "../lib/api";
  import { errorMessage } from "../lib/types";
  import { notify } from "../lib/toasts";
  import Icon from "../components/Icon.svelte";
  import Spinner from "../components/Spinner.svelte";

  interface Props {
    onSaved: () => void;
    onClose: () => void;
  }
  let { onSaved, onClose }: Props = $props();

  let title = $state("");
  let installPath = $state("");
  let saveRoot = $state("");
  let includeText = $state("**/*");
  let excludeText = $state("");
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function pickInstall() {
    try {
      const p = await open({ title: "Game .exe or install folder" });
      if (typeof p === "string") installPath = p;
    } catch (e) {
      error = errorMessage(e);
    }
  }

  async function pickSave() {
    try {
      const p = await open({ directory: true, title: "Save folder" });
      if (typeof p === "string") saveRoot = p;
    } catch (e) {
      error = errorMessage(e);
    }
  }

  const lines = (s: string) =>
    s
      .split("\n")
      .map((l) => l.trim())
      .filter(Boolean);

  async function submit() {
    error = null;
    const t = title.trim();
    const save = saveRoot.trim();
    if (!t) {
      error = "Give the game a name.";
      return;
    }
    if (!save) {
      error = "Pick the save folder.";
      return;
    }
    busy = true;
    try {
      await addCustomGame({
        title: t,
        install_path: installPath.trim() || null,
        save_root: save,
        include: lines(includeText),
        exclude: lines(excludeText),
      });
      notify.success(`${t} added.`);
      onSaved();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onClose();
  }

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onClose();
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="backdrop" onclick={onBackdropClick} role="presentation">
  <div
    class="dialog"
    role="dialog"
    tabindex="-1"
    aria-modal="true"
    aria-labelledby="add-game-title"
  >
    <div class="dialog-head">
      <h2 id="add-game-title">Add a game</h2>
      <button class="ghost sm icon-btn" onclick={onClose} aria-label="Close" type="button">
        <Icon name="x" size={16} />
      </button>
    </div>

    <div class="field">
      <label for="g-title">Name</label>
      <input id="g-title" bind:value={title} placeholder="e.g. Elden Ring" autocomplete="off" />
    </div>

    <div class="field">
      <label for="g-install">Install .exe / folder (optional, for detection)</label>
      <div class="path-row">
        <input id="g-install" bind:value={installPath} readonly placeholder="Not set" />
        <button class="sm" type="button" onclick={pickInstall}>
          <Icon name="roots" size={14} /> Browse
        </button>
      </div>
    </div>

    <div class="field">
      <label for="g-save">Save folder</label>
      <div class="path-row">
        <input id="g-save" bind:value={saveRoot} readonly placeholder="Required" />
        <button class="sm" type="button" onclick={pickSave}>
          <Icon name="roots" size={14} /> Browse
        </button>
      </div>
    </div>

    <div class="field">
      <label for="g-include">Include globs (one per line)</label>
      <textarea id="g-include" bind:value={includeText} rows="2"></textarea>
    </div>

    <div class="field">
      <label for="g-exclude">Exclude globs (one per line)</label>
      <textarea
        id="g-exclude"
        bind:value={excludeText}
        rows="2"
        placeholder="e.g. **/*.log"
      ></textarea>
    </div>

    {#if error}
      <div class="err">
        <Icon name="conflicts" size={15} />
        <span>{error}</span>
      </div>
    {/if}

    <div class="actions">
      <button type="button" onclick={onClose} disabled={busy}>Cancel</button>
      <button type="button" class="primary" onclick={submit} disabled={busy}>
        {#if busy}<Spinner size={14} />{:else}<Icon name="plus" size={15} />{/if}
        Add game
      </button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 24px;
  }
  .dialog {
    width: min(460px, 100%);
    max-height: min(640px, 90vh);
    overflow-y: auto;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    padding: 20px;
  }
  .dialog-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }
  .icon-btn {
    padding: 5px;
  }
  .field {
    margin-bottom: 14px;
  }
  .field:last-of-type {
    margin-bottom: 0;
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
  textarea {
    font-family: var(--mono);
    font-size: 12.5px;
    color: var(--text);
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 8px 11px;
    width: 100%;
    resize: vertical;
  }
  textarea:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
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
    margin-top: 14px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 20px;
  }
</style>
