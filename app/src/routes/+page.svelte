<script lang="ts">
  import { backend, usingMock } from "$lib/backend";
  import Onboarding from "$lib/Onboarding.svelte";
  import Home from "$lib/Home.svelte";
  import type { SetupState } from "$lib/types";

  let setup = $state<SetupState | null>(null);
  let error = $state("");

  $effect(() => {
    backend
      .getSetup()
      .then((s) => (setup = s))
      .catch((e) => (error = String(e)));
  });
</script>

{#if usingMock}
  <div class="mock" role="note">Preview with sample data (not running inside Logpose)</div>
{/if}

{#if error}
  <p class="error" role="alert">{error}</p>
{:else if !setup}
  <p class="loading">Loading…</p>
{:else if setup.needs_setup}
  <Onboarding initial={setup} onDone={(s) => (setup = s)} />
{:else}
  <Home />
{/if}

<style>
  /* Theme tokens (UI-003): light by default, dark from the system unless the user picked light,
     or dark when picked explicitly (`data-theme` on <html>). */
  :global(:root) {
    --bg: #ffffff;
    --surface: #f6f8fa;
    --fg: #1f2328;
    --muted: #656d76;
    --border: #d8dee4;
    --accent: #0969da;
    --green: #1a7f37;
    --green-bg: #dafbe1;
    --blue: #0969da;
    --blue-bg: #ddf4ff;
    --amber: #9a6700;
    --amber-bg: #fff8c5;
    --red: #cf222e;
    --red-bg: #ffebe9;
    --gray-bg: #eaeef2;
    --violet: #8250df;
    --shadow: 0 1px 2px rgb(31 35 40 / 8%);
    --shadow-hover: 0 3px 8px rgb(31 35 40 / 12%);
    color-scheme: light;
    font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root:not([data-theme="light"])) {
      --bg: #0d1117;
      --surface: #151b23;
      --fg: #e6edf3;
      --muted: #8d96a0;
      --border: #30363d;
      --accent: #4493f8;
      --green: #3fb950;
      --green-bg: #12261e;
      --blue: #4493f8;
      --blue-bg: #121d2f;
      --amber: #d29922;
      --amber-bg: #272115;
      --red: #f85149;
      --red-bg: #2d1517;
      --gray-bg: #1c2128;
      --violet: #ab7df8;
    --violet: #ab7df8;
      --shadow: 0 1px 2px rgb(0 0 0 / 30%);
      --shadow-hover: 0 3px 10px rgb(0 0 0 / 45%);
      color-scheme: dark;
    }
  }
  :global(:root[data-theme="dark"]) {
    --bg: #0d1117;
    --surface: #151b23;
    --fg: #e6edf3;
    --muted: #8d96a0;
    --border: #30363d;
    --accent: #4493f8;
    --green: #3fb950;
    --green-bg: #12261e;
    --blue: #4493f8;
    --blue-bg: #121d2f;
    --amber: #d29922;
    --amber-bg: #272115;
    --red: #f85149;
    --red-bg: #2d1517;
    --gray-bg: #1c2128;
    --violet: #ab7df8;
    --shadow: 0 1px 2px rgb(0 0 0 / 30%);
    --shadow-hover: 0 3px 10px rgb(0 0 0 / 45%);
    color-scheme: dark;
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
    color: var(--fg);
  }
  :global(button) {
    font: inherit;
    padding: 0.45rem 0.9rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }
  :global(button:hover:not(:disabled)) {
    border-color: var(--muted);
  }
  :global(button:disabled) {
    opacity: 0.55;
    cursor: default;
  }
  :global(button.primary) {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  :global(button.link) {
    border: none;
    background: none;
    color: var(--accent);
    padding: 0.2rem;
  }
  :global(:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  :global(code) {
    font-size: 0.85em;
  }
  /* Badges shared by the list, board and detail views. */
  :global(.badge),
  :global(.chip) {
    display: inline-block;
    padding: 0.1rem 0.5rem;
    border-radius: 999px;
    font-size: 0.75rem;
    line-height: 1.45;
    margin: 0.15rem 0.35rem 0.15rem 0;
    white-space: nowrap;
    border: 1px solid transparent;
  }
  :global(.chip),
  :global(.badge.neutral) {
    border-color: var(--border);
  }
  :global(.badge.neutral),
  :global(.muted) {
    color: var(--muted);
  }
  :global(.badge.fresh),
  :global(.badge.clean) {
    background: var(--green-bg);
    color: var(--green);
  }
  :global(.badge.cooling) {
    background: var(--blue-bg);
    color: var(--blue);
  }
  :global(.badge.stale),
  :global(.badge.tidy-up) {
    background: var(--amber-bg);
    color: var(--amber);
  }
  :global(.badge.dormant) {
    background: var(--gray-bg);
    color: var(--muted);
  }
  :global(.badge.abandoned-mid-way) {
    background: var(--red-bg);
    color: var(--red);
  }
  :global(.dot) {
    display: inline-block;
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    margin-right: 0.25rem;
  }
  :global(.dot.fresh) {
    background: var(--green);
  }
  :global(.dot.cooling) {
    background: var(--blue);
  }
  :global(.dot.stale) {
    background: var(--amber);
  }
  :global(.dot.dormant) {
    background: var(--muted);
  }
  /* A corner pill, so the preview banner doesn't change the full-height layout (UI-004). */
  .mock {
    position: fixed;
    right: 0.75rem;
    bottom: 0.75rem;
    z-index: 5;
    background: var(--amber-bg);
    color: var(--amber);
    border: 1px solid var(--amber);
    border-radius: 999px;
    font-size: 0.75rem;
    padding: 0.2rem 0.7rem;
    pointer-events: none;
  }
  .error,
  .loading {
    padding: 1.5rem;
  }
  .error {
    color: var(--red);
  }
</style>
