<script lang="ts">
  // DEL-002: remove a project and its leftovers (plan from DEL-001, decisions in DEC-009).
  // Order on screen follows the questions a user asks: can I get it back? what goes, and how big
  // is it? am I sure? Then each step's result and the space freed.
  import { backend } from "./backend";
  import type { RemovalGroup, RemovalItem, RemovalOutcome, RemovalPlan, RemovalStep, RepoRecovery } from "./types";

  let { path, displayName, onClose, onRemoved }: { path: string; displayName: string; onClose: () => void; onRemoved: () => void } =
    $props();

  const folderName = $derived(path.split(/[\\/]/).filter(Boolean).pop() ?? path);

  let plan = $state<RemovalPlan | null>(null);
  let loadError = $state("");
  let picked = $state<Set<string>>(new Set());
  let expanded = $state<Set<RemovalGroup>>(new Set());
  let ack = $state(false);
  let typed = $state("");
  let phase = $state<"plan" | "running" | "done">("plan");
  let steps = $state<Record<string, RemovalStep>>({});
  let outcome = $state<RemovalOutcome | null>(null);
  let runError = $state("");
  let remote = $state<Record<string, { state: "checking" } | { state: "ok"; branches: number } | { state: "error"; message: string }>>({});
  let copied = $state("");

  $effect(() => {
    backend
      .removalPlan(path)
      .then((p) => {
        plan = p;
        picked = new Set(p.items.filter((i) => i.selected || i.required).map((i) => i.id));
        for (const r of p.repos) {
          const url = primaryRemote(r)?.url;
          if (url && !remote[url]) {
            remote[url] = { state: "checking" };
            backend
              .checkRemote(url)
              .then((n) => (remote[url] = { state: "ok", branches: n }))
              .catch((e) => (remote[url] = { state: "error", message: String(e) }));
          }
        }
      })
      .catch((e) => (loadError = String(e)));
  });

  const groups: { key: RemovalGroup; title: string; hint: string }[] = [
    { key: "folder", title: "Project folder", hint: "Always moved to the Trash" },
    { key: "containers", title: "Docker containers", hint: "Stopped and removed, with their compose networks" },
    { key: "volumes", title: "Docker volumes", hint: "Data stored by the containers" },
    { key: "images", title: "Docker images", hint: "Only images no other project's container uses" },
    { key: "agent-logs", title: "Agent session logs", hint: "Kept unless you tick them" },
    { key: "logpose", title: "Logpose data", hint: "Always cleared" },
  ];
  const dockerGroups: RemovalGroup[] = ["containers", "volumes", "images"];
  const shownLimit = 5;

  const byGroup = $derived(
    Object.fromEntries(groups.map((g) => [g.key, plan?.items.filter((i) => i.group === g.key) ?? []])) as Record<RemovalGroup, RemovalItem[]>,
  );

  function fmt(bytes: number): string {
    if (bytes < 1e3) return `${bytes} B`;
    const units = ["kB", "MB", "GB", "TB"];
    let v = bytes / 1e3;
    let u = 0;
    while (v >= 1000 && u < units.length - 1) {
      v /= 1000;
      u++;
    }
    return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[u]}`;
  }

  const sum = (items: RemovalItem[]) => items.reduce((a, i) => a + i.bytes, 0);
  const chosen = (items: RemovalItem[]) => items.filter((i) => picked.has(i.id));
  const trashBytes = $derived(sum(chosen([...byGroup.folder, ...byGroup["agent-logs"]])));
  const dockerBytes = $derived(sum(chosen(dockerGroups.flatMap((g) => byGroup[g]))));

  function toggle(id: string) {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    picked = next;
  }

  /** Group checkbox: ticks or unticks the certain items; probable ones are ticked one by one. */
  function toggleGroup(key: RemovalGroup) {
    const certain = byGroup[key].filter((i) => !i.required && i.certainty === "certain");
    const all = certain.every((i) => picked.has(i.id));
    const next = new Set(picked);
    for (const i of certain) {
      if (all) next.delete(i.id);
      else next.add(i.id);
    }
    picked = next;
  }

  function groupState(key: RemovalGroup): "all" | "some" | "none" {
    const optional = byGroup[key].filter((i) => !i.required);
    const n = optional.filter((i) => picked.has(i.id)).length;
    return n === 0 ? "none" : n === optional.length ? "all" : "some";
  }

  function toggleExpanded(key: RemovalGroup) {
    const next = new Set(expanded);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expanded = next;
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = text;
      setTimeout(() => copied === text && (copied = ""), 1500);
    } catch {
      copied = "";
    }
  }

  function primaryRemote(r: RepoRecovery) {
    return r.remotes.find((x) => x.name === "origin") ?? r.remotes[0] ?? null;
  }

  /** What a clone of this repository would not bring back. */
  function lost(r: RepoRecovery): string[] {
    const out: string[] = [];
    const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
    if (r.uncommitted) out.push(`${plural(r.uncommitted, "uncommitted file")} (changed, new or staged)`);
    for (const b of r.branches) {
      if (b.upstream && b.ahead) out.push(`${plural(b.ahead, "unpushed commit")} on ${b.name}`);
      else if (!b.upstream && b.ahead) out.push(`branch ${b.name} exists only here (${plural(b.ahead, "commit")})`);
    }
    if (r.stashes) out.push(plural(r.stashes, "stash", "stashes"));
    return out;
  }

  const atRisk = $derived(plan?.repos.some((r) => lost(r).length > 0 || !primaryRemote(r)) || (plan?.repos.length === 0 && !!plan));
  const confirmed = $derived(typed.trim() === folderName && (!atRisk || ack));

  async function run() {
    if (!plan || !confirmed) return;
    phase = "running";
    runError = "";
    try {
      outcome = await backend.removeProject(path, [...picked], (s) => (steps[s.id] = s));
      for (const s of outcome.steps) steps[s.id] = s;
    } catch (e) {
      runError = String(e);
    }
    phase = "done";
  }

  const failed = $derived(Object.values(steps).filter((s) => !s.ok));
  const freedTrash = $derived(Object.values(steps).filter((s) => s.ok && s.trashed).reduce((a, s) => a + s.bytes, 0));
  const folderGone = $derived(steps.folder?.ok === true);
  const runOrder = $derived(
    plan
      ? [...plan.items]
          .filter((i) => picked.has(i.id))
          .sort((a, b) => order(a) - order(b))
      : [],
  );
  function order(i: RemovalItem): number {
    const ranks: Record<RemovalGroup, number> = { containers: 0, volumes: 2, images: 3, "agent-logs": 4, folder: 5, logpose: 6 };
    return ranks[i.group] + (i.id.startsWith("network:") ? 1 : 0);
  }

  let dialog = $state<HTMLElement | null>(null);
  $effect(() => {
    dialog?.focus();
  });

  function close() {
    if (phase === "running") return;
    if (phase === "done" && folderGone) onRemoved();
    else onClose();
  }
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && (e.stopPropagation(), close())} />

<div class="backdrop" role="presentation" onclick={close}></div>
<div class="dialog" role="dialog" aria-modal="true" aria-labelledby="remove-title" tabindex="-1" bind:this={dialog}>
  <header>
    <div>
      <h2 id="remove-title">Remove {displayName}</h2>
      <p class="path" title={path}>{path}</p>
    </div>
    <button class="close" onclick={close} aria-label="Close" disabled={phase === "running"}>✕</button>
  </header>

  {#if loadError}
    <p class="err" role="alert">{loadError}</p>
  {:else if !plan}
    <div class="loading" role="status">
      <span class="spinner" aria-hidden="true"></span>
      Measuring the folder, checking Docker and the agent logs… nothing is changed yet.
    </div>
  {:else if phase === "plan"}
    <div class="body">
      <section aria-labelledby="back-title">
        <h3 id="back-title">What you can get back</h3>
        {#each plan.repos as r (r.path)}
          {@const rem = primaryRemote(r)}
          {@const missing = lost(r)}
          <div class="repo">
            {#if plan.repos.length > 1}<p class="repo-path" title={r.path}>{r.path.replace(path, ".") || "."}</p>{/if}
            {#if rem}
              {@const st = remote[rem.url]}
              <div class="remote">
                <span class={`state ${st?.state ?? "checking"}`}>
                  {#if !st || st.state === "checking"}
                    <span class="spinner small" aria-hidden="true"></span> Checking the remote…
                  {:else if st.state === "ok"}
                    ✓ The remote answers ({st.branches} branch{st.branches === 1 ? "" : "es"}): committed and pushed work can be cloned again.
                  {:else}
                    ⚠ The remote did not answer: {st.message}
                  {/if}
                </span>
                <div class="clone">
                  <code>git clone {rem.url}</code>
                  <button class="small" onclick={() => copy(`git clone ${rem.url}`)}>{copied === `git clone ${rem.url}` ? "Copied" : "Copy"}</button>
                  {#if rem.web_url}<button class="small" onclick={() => backend.openUrl(rem.web_url!)}>Open {new URL(rem.web_url).host}</button>{/if}
                </div>
              </div>
            {:else}
              <p class="warn">No remote: once the Trash is emptied, nothing can bring this repository back.</p>
            {/if}
            {#if missing.length}
              <p class="warn-title">Not on the remote — only the Trash will have it:</p>
              <ul class="lost">{#each missing as m (m)}<li>{m}</li>{/each}</ul>
            {:else if rem}
              <p class="ok-line">Nothing is missing from the remote.</p>
            {/if}
            {#if r.error}<p class="muted small-note">{r.error}</p>{/if}
          </div>
        {:else}
          <p class="warn">Not a git repository: once the Trash is emptied, nothing can bring it back.</p>
        {/each}
      </section>

      <section aria-labelledby="what-title">
        <h3 id="what-title">What will be removed</h3>
        {#each groups as g (g.key)}
          {@const items = byGroup[g.key]}
          {@const isDocker = dockerGroups.includes(g.key)}
          {#if items.length || (g.key === "containers" && plan.docker.state !== "available")}
            {@const state = groupState(g.key)}
            {@const required = items.length > 0 && items.every((i) => i.required)}
            {@const open = expanded.has(g.key)}
            {@const shown = open ? items : items.slice(0, shownLimit)}
            <div class="group" class:off={!required && state === "none"}>
              <div class="group-head">
                {#if required}
                  <span class="lock" title={g.hint} aria-hidden="true">●</span>
                {:else if items.length}
                  <input
                    type="checkbox"
                    aria-label={`All certain ${g.title.toLowerCase()}`}
                    checked={state === "all"}
                    indeterminate={state === "some"}
                    onchange={() => toggleGroup(g.key)}
                  />
                {/if}
                <div class="group-title">
                  <strong>{isDocker && items.length === 0 ? "Docker" : g.title}</strong>
                  <span class="muted">{g.hint}</span>
                </div>
                {#if g.key === "logpose"}
                  <span class="size muted">cleared</span>
                {:else if items.length && state === "none" && !required}
                  <span class="size muted" title="Nothing ticked in this group">kept · {fmt(sum(items))}</span>
                {:else if items.length}
                  <span class="size" title="Ticked items in this group"
                    >{fmt(sum(chosen(items)))}{#if !required && sum(items) !== sum(chosen(items))}<span class="muted">&nbsp;of {fmt(sum(items))}</span>{/if}</span
                  >
                {/if}
              </div>
              {#if g.key === "containers" && plan.docker.state !== "available"}
                <p class="muted note">
                  {plan.docker.state === "missing"
                    ? "Docker is not installed: no Docker leftovers to look for."
                    : `Docker could not be queried (${plan.docker.state === "unreachable" ? plan.docker.message : ""}): containers, volumes and images are not listed.`}
                </p>
              {/if}
              {#if g.key === "folder"}
                {#if plan.build_outputs.length}
                  <p class="muted note">
                    Includes rebuildable output:
                    {#each plan.build_outputs.slice(0, 3) as o, k (o.path)}{k ? ", " : ""}<code>{o.path.replace(path + "/", "")}</code> {fmt(o.bytes)}{/each}{#if plan.build_outputs.length > 3}, and {plan.build_outputs.length - 3} more{/if}.
                  </p>
                {/if}
              {:else if g.key !== "logpose"}
                <ul class="items">
                  {#each shown as i (i.id)}
                    <li class:probable={i.certainty === "probable"}>
                      <label>
                        <input type="checkbox" checked={picked.has(i.id)} onchange={() => toggle(i.id)} />
                        <span class="item-main">
                          <span class="item-label">
                            {i.label}
                            {#if i.certainty === "probable"}<span class="tag">Check first</span>{/if}
                          </span>
                          {#if i.detail}<span class="muted item-detail">{i.detail}</span>{/if}
                        </span>
                        <span class="item-size">{i.bytes ? fmt(i.bytes) : ""}</span>
                      </label>
                      {#if i.checks.length}
                        <div class="checks">
                          {#each i.checks as c (c)}
                            <div class="cmd">
                              <code>{c}</code>
                              <button class="small" onclick={() => copy(c)}>{copied === c ? "Copied" : "Copy"}</button>
                            </div>
                          {/each}
                        </div>
                      {/if}
                    </li>
                  {/each}
                </ul>
                {#if items.length > shownLimit}
                  <button class="link more" onclick={() => toggleExpanded(g.key)}>
                    {open ? "Show fewer" : `Show all ${items.length}`}
                  </button>
                {/if}
              {/if}
            </div>
          {/if}
        {/each}
        {#if byGroup.images.length > 1}
          <p class="muted small-note">
            Images share layers, so the sizes above are what each frees on its own; the space actually freed is measured
            when the removal is done.
          </p>
        {/if}
      </section>
    </div>

    <footer>
      <p class="totals">
        <span><strong>{fmt(trashBytes)}</strong> to the Trash <span class="muted">(freed when you empty it)</span></span>
        {#if plan.docker.state === "available"}<span><strong>≈ {fmt(dockerBytes)}</strong> of Docker space freed</span>{/if}
      </p>
      {#if atRisk}
        <label class="ack">
          <input type="checkbox" bind:checked={ack} />
          I understand that the work listed as not on the remote will only be in the Trash.
        </label>
      {/if}
      <div class="confirm">
        <label for="confirm-name">Type <code>{folderName}</code> to confirm</label>
        <input id="confirm-name" bind:value={typed} autocomplete="off" spellcheck="false" onkeydown={(e) => e.key === "Enter" && run()} />
        <button class="danger" disabled={!confirmed} onclick={run}>Remove project</button>
        <button onclick={onClose}>Cancel</button>
      </div>
    </footer>
  {:else}
    <div class="body">
      <section aria-live="polite">
        <h3>{phase === "running" ? "Removing…" : failed.length || runError ? "Removal stopped" : "Removed"}</h3>
        <ol class="steps">
          {#each runOrder as i (i.id)}
            {@const s = steps[i.id]}
            <li class:ok={s?.ok} class:bad={s && !s.ok}>
              <span class="mark" aria-hidden="true">{#if !s}{#if phase === "running"}<span class="spinner small"></span>{:else}–{/if}{:else if s.ok}✓{:else}✗{/if}</span>
              <span class="step-label">
                {i.group === "folder" ? "Project folder to the Trash" : i.group === "logpose" ? "Logpose data cleared" : i.label}
                {#if s && !s.ok}<span class="err step-err">{s.error}</span>{/if}
              </span>
              <span class="item-size">{i.bytes ? fmt(i.bytes) : ""}</span>
            </li>
          {/each}
        </ol>
        {#if runError}<p class="err" role="alert">{runError}</p>{/if}
      </section>
      {#if phase === "done"}
        <div class="summary" class:partial={failed.length > 0 || !!runError}>
          {#if outcome?.docker_freed != null}
            <p><strong>{fmt(outcome.docker_freed)}</strong> of Docker space freed (measured)</p>
          {/if}
          <p><strong>{fmt(freedTrash)}</strong> moved to the Trash — empty the Trash to free it</p>
          {#if failed.length}
            <p>
              {failed.length} step{failed.length === 1 ? "" : "s"} failed.
              {folderGone ? "" : "The folder and Logpose's data were left in place, so you can fix the problem and try again."}
            </p>
          {/if}
        </div>
      {/if}
    </div>
    <footer>
      <div class="confirm end">
        <button class="primary" onclick={close} disabled={phase === "running"}>{folderGone ? "Done" : "Close"}</button>
      </div>
    </footer>
  {/if}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.45);
    z-index: 20;
  }
  .dialog {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(46rem, calc(100vw - 2rem));
    max-height: calc(100vh - 3rem);
    display: flex;
    flex-direction: column;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 12px;
    box-shadow: 0 20px 60px rgb(0 0 0 / 35%);
    z-index: 21;
    font-size: 0.9rem;
    outline: none;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: start;
    gap: 1rem;
    padding: 1.1rem 1.25rem 0.8rem;
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: 1.15rem;
  }
  .path {
    margin: 0.3rem 0 0;
    color: var(--muted);
    font-size: 0.78rem;
    overflow-wrap: anywhere;
  }
  .close {
    border: none;
    background: none;
    color: var(--muted);
    padding: 0.3rem 0.5rem;
  }
  .body {
    overflow-y: auto;
    padding: 0 1.25rem 1rem;
  }
  h3 {
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    margin: 1.1rem 0 0.5rem;
  }
  .loading {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 2rem 1.25rem;
    color: var(--muted);
  }
  .spinner {
    width: 1rem;
    height: 1rem;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    display: inline-block;
    vertical-align: -0.15rem;
    flex: none;
  }
  .spinner.small {
    width: 0.75rem;
    height: 0.75rem;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .repo {
    border: 1px solid var(--border);
    border-radius: 9px;
    padding: 0.8rem 0.9rem;
    margin-bottom: 0.6rem;
  }
  .repo-path {
    margin: 0 0 0.4rem;
    font-weight: 600;
    font-size: 0.82rem;
  }
  .state.ok {
    color: var(--green);
  }
  .state.error {
    color: var(--amber);
  }
  .state.checking {
    color: var(--muted);
  }
  .clone,
  .cmd {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-top: 0.45rem;
    flex-wrap: wrap;
  }
  .clone code,
  .cmd code {
    flex: 1;
    min-width: 0;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.3rem 0.5rem;
    font-size: 0.78rem;
    overflow-x: auto;
    white-space: nowrap;
  }
  button.small {
    padding: 0.25rem 0.6rem;
    font-size: 0.78rem;
    white-space: nowrap;
  }
  .warn,
  .warn-title {
    color: var(--amber);
    margin: 0.5rem 0 0.2rem;
  }
  .warn-title {
    font-weight: 600;
    font-size: 0.85rem;
  }
  .lost {
    margin: 0.3rem 0 0;
    padding-left: 1.2rem;
    color: var(--amber);
  }
  .lost li + li {
    margin-top: 0.2rem;
  }
  .ok-line {
    color: var(--green);
    margin: 0.5rem 0 0;
  }
  .group {
    border: 1px solid var(--border);
    border-radius: 9px;
    padding: 0.75rem 0.9rem;
    margin-bottom: 0.6rem;
  }
  .group.off .group-title strong {
    color: var(--muted);
  }
  .group-head {
    display: flex;
    align-items: center;
    gap: 0.65rem;
  }
  .group-head input {
    margin: 0;
  }
  .lock {
    color: var(--red);
    font-size: 0.7rem;
    width: 13px;
    text-align: center;
  }
  .group-title {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .group-title .muted {
    font-size: 0.78rem;
  }
  .size {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    white-space: nowrap;
  }
  .note {
    margin: 0.45rem 0 0 1.6rem;
    font-size: 0.8rem;
  }
  .items {
    list-style: none;
    margin: 0.6rem 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .items label {
    display: flex;
    align-items: start;
    gap: 0.65rem;
    padding: 0.35rem 0.3rem 0.35rem 1.6rem;
    border-radius: 6px;
    cursor: pointer;
  }
  .items label:hover {
    background: var(--surface);
  }
  .items input {
    margin: 0.2rem 0 0;
  }
  .item-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .item-label {
    overflow-wrap: anywhere;
  }
  .item-detail {
    font-size: 0.78rem;
  }
  .item-size {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    color: var(--muted);
    font-size: 0.82rem;
  }
  .probable {
    border-left: 3px solid var(--amber);
    border-radius: 0 6px 6px 0;
    background: color-mix(in srgb, var(--amber-bg) 45%, transparent);
  }
  .tag {
    display: inline-block;
    margin-left: 0.4rem;
    padding: 0 0.45rem;
    border-radius: 999px;
    font-size: 0.7rem;
    font-weight: 600;
    color: var(--amber);
    border: 1px solid var(--amber);
    vertical-align: 0.1rem;
  }
  .checks {
    padding: 0 0.5rem 0.5rem 3.2rem;
  }
  .more {
    margin: 0.3rem 0 0 1.6rem;
    font-size: 0.82rem;
  }
  .small-note {
    font-size: 0.78rem;
  }
  footer {
    border-top: 1px solid var(--border);
    padding: 0.8rem 1.25rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .totals {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem 1.25rem;
  }
  .ack {
    display: flex;
    gap: 0.5rem;
    align-items: start;
    color: var(--amber);
    font-size: 0.85rem;
  }
  .ack input {
    margin-top: 0.15rem;
  }
  .confirm {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .confirm.end {
    justify-content: flex-end;
  }
  .confirm label {
    font-size: 0.85rem;
  }
  .confirm input {
    font: inherit;
    flex: 1;
    min-width: 10rem;
    padding: 0.4rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--fg);
  }
  button.danger {
    background: var(--red);
    border-color: var(--red);
    color: #fff;
    font-weight: 600;
  }
  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .steps li {
    display: flex;
    align-items: start;
    gap: 0.6rem;
    padding: 0.3rem 0.2rem;
  }
  .mark {
    width: 1.1rem;
    text-align: center;
    color: var(--muted);
  }
  .steps li.ok .mark {
    color: var(--green);
  }
  .steps li.bad .mark,
  .err {
    color: var(--red);
  }
  .step-label {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow-wrap: anywhere;
  }
  .step-err {
    font-size: 0.8rem;
  }
  .summary {
    margin-top: 1rem;
    padding: 0.8rem 1rem;
    border-radius: 9px;
    background: var(--green-bg);
    color: var(--green);
  }
  .summary.partial {
    background: var(--amber-bg);
    color: var(--amber);
  }
  .summary p {
    margin: 0.15rem 0;
  }
</style>
