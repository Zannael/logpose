<script lang="ts">
  // SET-001: edit roots, exclusions and aliases. Saving writes them back into config.toml (other
  // keys and comments are kept) and triggers a rescan.
  import { backend } from "./backend";
  import { setTheme, theme, type Theme } from "./theme.svelte";
  import type { AgentCheck, Locations, ProjectReport } from "./types";

  let { projects, onSaved }: { projects: ProjectReport[]; onSaved: () => Promise<number | null> } = $props();

  let configPath = $state("");
  let roots = $state<string[]>([]);
  let exclude = $state<string[]>([]);
  let aliases = $state<{ path: string; name: string }[]>([]);
  let saved = $state<Locations | null>(null);
  let error = $state("");
  let status = $state("");
  let saving = $state(false);
  let newAliasPath = $state("");

  function load(loc: Locations, path: string) {
    configPath = path;
    roots = [...loc.roots];
    exclude = [...loc.exclude];
    aliases = Object.entries(loc.aliases).map(([path, name]) => ({ path, name }));
    saved = structuredClone(loc);
  }

  $effect(() => {
    backend
      .getSettings()
      .then((s) => load(s.locations, s.config_path))
      .catch((e) => (error = String(e)));
  });

  const current = $derived<Locations>({
    roots,
    exclude,
    aliases: Object.fromEntries(aliases.filter((a) => a.name.trim()).map((a) => [a.path, a.name.trim()])),
  });
  const dirty = $derived(saved !== null && JSON.stringify(current) !== JSON.stringify(saved));
  const unaliased = $derived(
    [...projects]
      .filter((p) => !aliases.some((a) => a.path === p.project.path))
      .sort((a, b) => a.project.title.localeCompare(b.project.title)),
  );
  const titleOf = (path: string) => projects.find((p) => p.project.path === path)?.project.title ?? path.split("/").pop();

  async function addRoot() {
    const picked = await backend.pickFolder();
    if (picked && !roots.includes(picked)) roots = [...roots, picked];
  }

  function addAlias() {
    if (!newAliasPath) return;
    aliases = [...aliases, { path: newAliasPath, name: titleOf(newAliasPath) ?? "" }];
    newAliasPath = "";
  }

  const agents = [
    { id: "claude-code" as const, label: "Claude Code" },
    { id: "codex" as const, label: "Codex" },
  ];
  let checks = $state<Record<string, AgentCheck | "running">>({});

  async function check(agent: "claude-code" | "codex") {
    checks[agent] = "running";
    try {
      checks[agent] = await backend.checkAgent(agent);
    } catch (e) {
      checks[agent] = { agent, program: null, ok: false, model: null, tokens: 0, millis: 0, error: String(e) };
    }
  }

  let trayOn = $state<boolean | null>(null);
  $effect(() => {
    backend.trayEnabled().then((v) => (trayOn = v)).catch(() => {});
  });
  async function toggleTray() {
    try {
      trayOn = await backend.setTrayEnabled(!trayOn);
    } catch (e) {
      error = String(e);
    }
  }

  let digestOn = $state<boolean | null>(null);
  $effect(() => {
    backend.digestEnabled().then((v) => (digestOn = v)).catch(() => {});
  });
  async function toggleDigest() {
    try {
      digestOn = await backend.setDigestEnabled(!digestOn);
    } catch (e) {
      error = String(e);
    }
  }

  async function save() {
    saving = true;
    error = "";
    status = "";
    try {
      const s = await backend.saveSettings({ ...current, exclude: exclude.map((g) => g.trim()).filter(Boolean) });
      load(s.locations, s.config_path);
      status = "Saved. Rescanning…";
      const found = await onSaved();
      status = found === null ? "" : `Saved and rescanned: ${found} projects.`;
      if (found === null) error = "Saved, but the rescan failed (see the message at the top).";
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }
</script>

<div class="settings">
  <header>
    <h2>Settings</h2>
    <p class="muted">Saved to <code>{configPath}</code>. Other settings and comments in that file are kept.</p>
  </header>

  <section>
    <h3>Appearance</h3>
    <div class="seg" role="radiogroup" aria-label="Theme">
      {#each [["system", "Same as the system"], ["light", "Light"], ["dark", "Dark"]] as [t, label] (t)}
        <label class:on={theme.choice === t}>
          <input type="radio" name="theme" value={t} checked={theme.choice === t} onchange={() => setTheme(t as Theme)} />
          {label}
        </label>
      {/each}
    </div>
    <p class="muted">Remembered on this computer and applied immediately. The sun/moon button in the header switches too.</p>
  </section>

  <section>
    <h3>Project folders</h3>
    <p class="muted">Logpose looks for projects inside these folders.</p>
    <ul class="rows">
      {#each roots as root, i (root)}
        <li>
          <code class="grow">{root}</code>
          <button class="link" onclick={() => (roots = roots.filter((_, k) => k !== i))} aria-label={`Remove ${root}`}>Remove</button>
        </li>
      {/each}
    </ul>
    {#if !roots.length}<p class="warn">Add at least one folder.</p>{/if}
    <button onclick={addRoot}>Add folder…</button>
  </section>

  <section>
    <h3>Exclusions</h3>
    <p class="muted">Glob patterns of paths to skip, e.g. <code>~/work/**</code> or <code>**/client-*</code>.</p>
    <ul class="rows">
      {#each exclude as _, i (i)}
        <li>
          <input class="grow" bind:value={exclude[i]} aria-label={`Exclusion ${i + 1}`} placeholder="~/path/**" />
          <button class="link" onclick={() => (exclude = exclude.filter((_, k) => k !== i))} aria-label={`Remove exclusion ${i + 1}`}>Remove</button>
        </li>
      {/each}
    </ul>
    <button onclick={() => (exclude = [...exclude, ""])}>Add exclusion</button>
  </section>

  <section>
    <h3>Display names</h3>
    <p class="muted">A name shown instead of the folder name.</p>
    <ul class="rows">
      {#each aliases as a, i (a.path)}
        <li>
          <span class="who" title={a.path}>{titleOf(a.path)}</span>
          <input class="grow" bind:value={aliases[i].name} aria-label={`Display name for ${titleOf(a.path)}`} />
          <button class="link" onclick={() => (aliases = aliases.filter((_, k) => k !== i))} aria-label={`Remove name for ${titleOf(a.path)}`}>Remove</button>
        </li>
      {/each}
    </ul>
    <div class="add">
      <select bind:value={newAliasPath} aria-label="Project to rename">
        <option value="">Choose a project…</option>
        {#each unaliased as p (p.project.path)}<option value={p.project.path}>{p.project.title}</option>{/each}
      </select>
      <button onclick={addAlias} disabled={!newAliasPath}>Add name</button>
    </div>
  </section>

  <section>
    <h3>Tray mode</h3>
    <label class="check">
      <input type="checkbox" checked={!!trayOn} disabled={trayOn === null} onchange={toggleTray} />
      Keep Logpose in the tray and rescan when projects change
    </label>
    <p class="muted">
      Closing the window hides it to the tray (use the tray menu to show it or quit). Only project folders, their
      <code>.git</code> folders and the agent logs are watched; a rescan runs at most once a minute and never calls an agent.
    </p>
  </section>

  <section>
    <h3>Weekly digest</h3>
    <label class="check">
      <input type="checkbox" checked={!!digestOn} disabled={digestOn === null} onchange={toggleDigest} />
      Once a week, when you open Logpose, show a notification with the 3 most neglected projects
    </label>
    <p class="muted">Archived and Maintenance projects are never listed. Saved immediately.</p>
  </section>

  <section>
    <h3>Agents</h3>
    <p class="muted">
      Runs a tiny made-up project through the agent to catch login problems or changed CLI flags. No project data is
      sent; it spends a few tokens.
    </p>
    <ul class="rows">
      {#each agents as a (a.id)}
        {@const c = checks[a.id]}
        <li class="agent">
          <span class="who">{a.label}</span>
          <span class="grow" role="status">
            {#if c === "running"}
              <span class="muted">Testing…</span>
            {:else if c?.ok}
              <span class="ok">✓ Works</span>
              <span class="muted">· {c.model ?? "default model"} · {c.tokens.toLocaleString()} tokens · {(c.millis / 1000).toFixed(1)} s</span>
            {:else if c}
              <span class="error">✗ Failed:</span> <code class="detail">{c.error}</code>
            {/if}
            {#if c && c !== "running" && c.program}<span class="muted small">{c.program}</span>{/if}
          </span>
          <button onclick={() => check(a.id)} disabled={c === "running"}>Test</button>
        </li>
      {/each}
    </ul>
  </section>

  <footer>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if status}<p class="ok" role="status">{status}</p>{/if}
    <button class="primary" onclick={save} disabled={saving || !dirty || !roots.length}>
      {saving ? "Saving…" : "Save and rescan"}
    </button>
    {#if dirty}<span class="unsaved">Unsaved changes to folders, exclusions or names</span>{:else}<span class="muted">Folders, exclusions and names are saved with this button; everything else is saved at once.</span>{/if}
  </footer>
</div>

<style>
  .settings {
    max-width: 46rem;
    padding: 1rem 1.5rem 0;
  }
  h2 {
    margin: 0 0 0.25rem;
    font-size: 1.1rem;
  }
  h3 {
    font-size: 0.9rem;
    margin: 0 0 0.45rem;
  }
  section {
    margin-top: 1.5rem;
  }
  p {
    margin: 0 0 0.6rem;
    font-size: 0.85rem;
  }
  .rows {
    list-style: none;
    margin: 0 0 0.6rem;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .rows li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.35rem 0.6rem;
  }
  .grow {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .who {
    width: 10rem;
    font-weight: 600;
    font-size: 0.85rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  input,
  select {
    font: inherit;
    font-size: 0.85rem;
    padding: 0.3rem 0.45rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
  }
  .add {
    display: flex;
    gap: 0.6rem;
  }
  /* Kept in view so unsaved folder/exclusion/name edits are never lost below the fold (UI-002). */
  footer {
    position: sticky;
    bottom: 0;
    margin-top: 2rem;
    padding: 0.75rem 0;
    background: var(--bg);
    border-top: 1px solid var(--border);
    display: flex;
    align-items: center;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .unsaved {
    color: var(--amber);
  }
  footer p {
    width: 100%;
    margin: 0;
  }
  .agent {
    align-items: flex-start;
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
    margin-bottom: 0.4rem;
  }
  .seg label {
    padding: 0.35rem 0.8rem;
    font-size: 0.85rem;
    cursor: pointer;
    color: var(--muted);
  }
  .seg label + label {
    border-left: 1px solid var(--border);
  }
  .seg label.on {
    background: var(--gray-bg);
    color: var(--fg);
    font-weight: 600;
  }
  .seg label:has(input:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .seg input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .check {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    font-size: 0.85rem;
    margin-bottom: 0.3rem;
  }
  .agent .who {
    padding-top: 0.35rem;
  }
  .agent .grow {
    padding-top: 0.35rem;
    font-size: 0.85rem;
  }
  .detail {
    display: block;
    white-space: pre-wrap;
    margin-top: 0.2rem;
  }
  .small {
    display: block;
    font-size: 0.75rem;
  }
  .error,
  .warn {
    color: var(--red);
  }
  .ok {
    color: var(--green);
  }
</style>
