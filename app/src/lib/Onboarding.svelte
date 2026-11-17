<script lang="ts">
  // ONBOARD-001: blocking first-launch setup. No built-in defaults: the user picks every root.
  import { untrack } from "svelte";
  import { backend } from "./backend";
  import type { SetupState } from "./types";

  let { initial, onDone }: { initial: SetupState; onDone: (s: SetupState) => void } = $props();

  // Editable copy of the configured roots, taken once.
  let roots = $state<string[]>(untrack(() => [...initial.roots]));
  let error = $state("");
  let saving = $state(false);

  async function addFolder() {
    error = "";
    const picked = await backend.pickFolder();
    if (picked && !roots.includes(picked)) roots = [...roots, picked];
  }

  function remove(root: string) {
    roots = roots.filter((r) => r !== root);
  }

  async function save() {
    saving = true;
    error = "";
    try {
      onDone(await backend.saveRoots(roots));
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }
</script>

<section class="setup" aria-labelledby="setup-title">
  <h1 id="setup-title">Where do your projects live?</h1>
  <p class="lead">
    Pick the folders that contain your projects. Logpose looks a few levels deep inside each one for git repos and
    agent-managed folders. You can change this later in settings.
  </p>

  <ul class="roots">
    {#each roots as root (root)}
      <li>
        <code>{root}</code>
        <button class="link" onclick={() => remove(root)} aria-label={`Remove ${root}`}>Remove</button>
      </li>
    {:else}
      <li class="empty">No folders yet.</li>
    {/each}
  </ul>

  <div class="actions">
    <button onclick={addFolder}>Add folder…</button>
    <button class="primary" onclick={save} disabled={roots.length === 0 || saving}>
      {saving ? "Saving…" : "Start scanning"}
    </button>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  <p class="hint">Saved to <code>{initial.config_path}</code></p>
</section>

<style>
  .setup {
    max-width: 36rem;
    margin: 12vh auto 0;
    padding: 0 1rem;
  }
  h1 {
    font-size: 1.6rem;
    margin: 0 0 0.5rem;
  }
  .lead {
    color: var(--muted);
    line-height: 1.5;
  }
  .roots {
    list-style: none;
    padding: 0;
    margin: 1.5rem 0;
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .roots li {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.6rem 0.8rem;
    border-bottom: 1px solid var(--border);
  }
  .roots li:last-child {
    border-bottom: none;
  }
  .empty {
    color: var(--muted);
  }
  .actions {
    display: flex;
    gap: 0.6rem;
  }
  .error {
    color: var(--red);
  }
  .hint {
    color: var(--muted);
    font-size: 0.85rem;
    margin-top: 1.5rem;
  }
</style>
