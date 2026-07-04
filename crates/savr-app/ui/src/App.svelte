<script lang="ts">
  import { onMount } from "svelte";
  import Icon from "./components/Icon.svelte";
  import ToastHost from "./components/ToastHost.svelte";
  import Dashboard from "./views/Dashboard.svelte";
  import Games from "./views/Games.svelte";
  import Conflicts from "./views/Conflicts.svelte";
  import Roots from "./views/Roots.svelte";
  import Settings from "./views/Settings.svelte";
  import Pairing from "./views/Pairing.svelte";
  import { checkForUpdates } from "./lib/updater";

  type ViewId = "dashboard" | "games" | "conflicts" | "roots" | "settings";

  const NAV: { id: ViewId; label: string; icon: string }[] = [
    { id: "dashboard", label: "Dashboard", icon: "dashboard" },
    { id: "games", label: "Games", icon: "games" },
    { id: "conflicts", label: "Conflicts", icon: "conflicts" },
    { id: "roots", label: "Roots", icon: "roots" },
    { id: "settings", label: "Settings", icon: "settings" },
  ];

  const ONBOARD_KEY = "savr.onboarded";
  const THEME_KEY = "savr.theme";

  let view = $state<ViewId>("dashboard");
  let onboarded = $state(localStorage.getItem(ONBOARD_KEY) === "1");
  let theme = $state<"dark" | "light">(
    (localStorage.getItem(THEME_KEY) as "dark" | "light") || "dark",
  );

  function applyTheme() {
    document.documentElement.setAttribute("data-theme", theme);
  }
  function toggleTheme() {
    theme = theme === "dark" ? "light" : "dark";
    localStorage.setItem(THEME_KEY, theme);
    applyTheme();
  }

  function completeOnboarding() {
    localStorage.setItem(ONBOARD_KEY, "1");
    onboarded = true;
    view = "dashboard";
  }

  onMount(() => {
    applyTheme();
    // Auto-check for updates on launch (silent; install is user-confirmed).
    checkForUpdates({ silent: true });
  });
</script>

{#if !onboarded}
  <Pairing onPaired={completeOnboarding} onSkip={completeOnboarding} />
{:else}
  <div class="shell">
    <aside class="sidebar">
      <div class="logo">
        <span class="mark"><Icon name="box" size={18} /></span>
        <span class="name">Savr</span>
      </div>
      <nav>
        {#each NAV as item}
          <button
            class="nav-item"
            class:active={view === item.id}
            onclick={() => (view = item.id)}
          >
            <Icon name={item.icon} size={17} />
            <span>{item.label}</span>
          </button>
        {/each}
      </nav>
      <div class="foot">
        <button class="nav-item subtle" onclick={toggleTheme}>
          <Icon name={theme === "dark" ? "sun" : "moon"} size={16} />
          <span>{theme === "dark" ? "Light" : "Dark"} theme</span>
        </button>
      </div>
    </aside>

    <main class="content">
      {#if view === "dashboard"}
        <Dashboard />
      {:else if view === "games"}
        <Games />
      {:else if view === "conflicts"}
        <Conflicts />
      {:else if view === "roots"}
        <Roots />
      {:else if view === "settings"}
        <Settings {theme} onToggleTheme={toggleTheme} />
      {/if}
    </main>
  </div>
{/if}

<ToastHost />

<style>
  .shell {
    display: grid;
    grid-template-columns: 216px 1fr;
    height: 100vh;
  }
  .sidebar {
    display: flex;
    flex-direction: column;
    background: var(--bg-elev);
    border-right: 1px solid var(--border);
    padding: 16px 12px;
  }
  .logo {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 8px 18px;
  }
  .mark {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
  }
  .name {
    font-size: 17px;
    font-weight: 700;
    letter-spacing: -0.01em;
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 11px;
    width: 100%;
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    padding: 9px 11px;
    color: var(--text-muted);
    font-size: 13.5px;
    font-weight: 550;
  }
  .nav-item:hover {
    background: var(--panel-2);
    color: var(--text);
  }
  .nav-item.active {
    background: var(--accent-soft);
    color: var(--accent);
    border-color: transparent;
  }
  .foot {
    margin-top: auto;
  }
  .subtle {
    color: var(--text-dim);
  }
  .content {
    overflow-y: auto;
    padding: 28px 32px;
  }
  @media (max-width: 680px) {
    .shell {
      grid-template-columns: 64px 1fr;
    }
    .name,
    .nav-item span {
      display: none;
    }
    .nav-item {
      justify-content: center;
    }
  }
</style>
