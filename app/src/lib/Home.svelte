<script lang="ts">
  // Main screen: header with scan summary, Board / List toggle, and the selected view.
  import { backend } from "./backend";
  import Board from "./Board.svelte";
  import Detail from "./Detail.svelte";
  import Picker from "./Picker.svelte";
  import { bucketText } from "./labels";
  import ProjectList from "./ProjectList.svelte";
  import Settings from "./Settings.svelte";
  import Stats from "./Stats.svelte";
  import { setTheme, shownTheme } from "./theme.svelte";
  import type { Agent, BoardColumn, PickerInfo, BoardRow, Bucket, ProjectReport, ScanReport, SynthState } from "./types";

  type View = "board" | "list" | "stats" | "settings";
  const viewKey = "logpose-view";

  let report = $state<ScanReport | null>(null);
  let board = $state<BoardRow[]>([]);
  let error = $state("");
  let scanning = $state(false);
  let digestChecked = false;

  // SYN-010: syntheses per project path, streamed in as they complete.
  const agentKey = "logpose-agent";
  let synth = $state<Record<string, SynthState>>({});
  let agent = $state<Agent>(loadAgent());
  let summarizing = $state(false);
  let synthNote = $state("");

  function loadAgent(): Agent {
    try {
      return localStorage.getItem(agentKey) === "codex" ? "codex" : "claude-code";
    } catch {
      return "claude-code";
    }
  }

  function setAgent(a: Agent) {
    agent = a;
    try {
      localStorage.setItem(agentKey, a);
    } catch {
      // Remembering the agent is a convenience only.
    }
  }

  const empty = (): SynthState => ({ synthesis: null, source: null, outdated: false, running: false, error: null });

  async function loadKnown() {
    try {
      const known = await backend.knownSyntheses();
      const next: Record<string, SynthState> = {};
      for (const { path, known: k } of known) {
        next[path] = { ...empty(), synthesis: k.synthesis, source: k.source, outdated: k.outdated };
      }
      // Keep runs in progress.
      for (const [path, st] of Object.entries(synth)) if (st.running) next[path] = st;
      synth = next;
    } catch (e) {
      synthNote = `Could not load saved summaries: ${e}`;
    }
  }

  /** Summarize every project (`paths` null; unchanged ones come from the cache) or force-refresh some. */
  async function summarize(paths: string[] | null, force: boolean) {
    const targets = paths ?? projects.map((p) => p.project.path);
    for (const path of targets) synth[path] = { ...(synth[path] ?? empty()), running: true, error: null };
    if (!paths) summarizing = true;
    synthNote = "";
    try {
      const s = await backend.synthesize(agent, paths, force, (u) => {
        synth[u.path] = u.ok
          ? { synthesis: u.synthesis, source: u.source, outdated: false, running: false, error: null }
          : { ...(synth[u.path] ?? empty()), running: false, error: u.error };
      });
      const parts = [`${s.ran} summarized`, `${s.cached} from cache`];
      if (s.next_md) parts.push(`${s.next_md} from NEXT.md`);
      if (s.failed) parts.push(`${s.failed} failed`);
      synthNote = parts.join(" · ");
    } catch (e) {
      synthNote = String(e);
    } finally {
      for (const path of targets) if (synth[path]?.running) synth[path] = { ...synth[path], running: false };
      if (!paths) summarizing = false;
    }
  }

  const refresh = (path: string) => summarize([path], true);

  // SYN-009: once per app start, after the first scan, ask which agent to spend on (or run the
  // configured default without asking).
  let picker = $state<PickerInfo | null>(null);
  let pickerChecked = false;

  async function startupPick() {
    try {
      const info = await backend.agentPicker();
      if (info.default_agent === "ask") picker = info;
      else pick(info.default_agent === "codex" ? "codex" : "claude-code");
    } catch (e) {
      synthNote = `Could not read agent usage: ${e}`;
    }
  }

  function pick(a: Agent) {
    picker = null;
    setAgent(a);
    summarize(null, false);
  }
  let view = $state<View>(loadView());

  function loadView(): View {
    try {
      const v = localStorage.getItem(viewKey);
      return v === "list" || v === "stats" ? v : "board";
    } catch {
      return "board";
    }
  }

  function setView(v: View) {
    view = v;
    // Settings is not remembered as the start view.
    if (v === "settings") return;
    try {
      localStorage.setItem(viewKey, v);
    } catch {
      // Remembering the view is a convenience only.
    }
  }

  const projects = $derived(report?.projects ?? []);

  // UI-002: the sidebar holds views and actions; it collapses to an icon rail (remembered). UI-004: by
  // default it starts collapsed only when the five columns would not fit next to it.
  // Five 13 rem columns + four 0.75 rem gaps + 1.5 rem padding each side (see Board.svelte and the
  // app's IDEAL_WIDTH).
  const boardMin = 5 * 208 + 4 * 12 + 2 * 24;
  const collapsedKey = "logpose-sidebar-collapsed";
  let collapsed = $state(loadCollapsed());
  function loadCollapsed(): boolean {
    try {
      const v = localStorage.getItem(collapsedKey);
      if (v !== null) return v === "1";
    } catch {
      // Remembering the sidebar is a convenience only.
    }
    return innerWidth < loadWidth() + 1 + boardMin;
  }
  function toggleCollapsed() {
    collapsed = !collapsed;
    try {
      localStorage.setItem(collapsedKey, collapsed ? "1" : "0");
    } catch {
      // See loadCollapsed.
    }
  }

  // UI-002: one search for the Board and the List, over names, paths and summaries.
  let query = $state("");
  let searchInput = $state<HTMLInputElement | null>(null);
  // Status filter in the sidebar: every status is on by default; clicking one turns it off or on,
  // "All projects" turns them all on again. Projects without a status show only when all are on.
  const buckets: Bucket[] = ["fresh", "cooling", "stale", "dormant"];
  let hidden = $state<Bucket[]>([]);
  const toggleBucket = (b: Bucket) =>
    (hidden = hidden.includes(b) ? hidden.filter((x) => x !== b) : [...hidden, b]);
  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return projects.filter((p) => {
      if (hidden.length && (!p.staleness || hidden.includes(p.staleness.bucket))) return false;
      if (!q) return true;
      return [p.project.alias, p.project.title, p.project.path, synth[p.project.path]?.synthesis?.description].some((t) =>
        t?.toLowerCase().includes(q),
      );
    });
  });
  const filtering = $derived(!!query.trim() || hidden.length > 0);

  // Sidebar width: drag the right edge (remembered).
  const widthKey = "logpose-sidebar-width";
  const minWidth = 200;
  const maxWidth = 360;
  let sideWidth = $state(loadWidth());
  function loadWidth(): number {
    try {
      const v = Number(localStorage.getItem(widthKey));
      if (v >= minWidth && v <= maxWidth) return v;
    } catch {
      // Remembering the width is a convenience only.
    }
    return 248;
  }
  function startResize(e: PointerEvent) {
    const handle = e.currentTarget as HTMLElement;
    handle.setPointerCapture(e.pointerId);
    const x0 = e.clientX;
    const w0 = sideWidth;
    const move = (m: PointerEvent) => (sideWidth = Math.round(Math.min(maxWidth, Math.max(minWidth, w0 + m.clientX - x0))));
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      try {
        localStorage.setItem(widthKey, String(sideWidth));
      } catch {
        // See loadWidth.
      }
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
  }
  function resizeKey(e: KeyboardEvent) {
    const step = e.key === "ArrowLeft" ? -16 : e.key === "ArrowRight" ? 16 : 0;
    if (!step) return;
    e.preventDefault();
    sideWidth = Math.min(maxWidth, Math.max(minWidth, sideWidth + step));
    try {
      localStorage.setItem(widthKey, String(sideWidth));
    } catch {
      // See loadWidth.
    }
  }

  function globalKey(e: KeyboardEvent) {
    // "/" focuses the search, as in most web apps, unless the user is typing somewhere.
    const t = e.target as HTMLElement;
    if (e.key !== "/" || t.closest("input, textarea, select, [contenteditable]") || selectedPath) return;
    if (view !== "board" && view !== "list") return;
    e.preventDefault();
    searchInput?.focus();
  }
  const counts = $derived(
    projects.reduce<Record<string, number>>((acc, p) => {
      const b = p.staleness?.bucket ?? "unknown";
      acc[b] = (acc[b] ?? 0) + 1;
      return acc;
    }, {}),
  );

  /** Scan and reload the board; resolves to the number of projects found, or null on failure. */
  export async function rescan(): Promise<number | null> {
    scanning = true;
    error = "";
    try {
      report = await backend.scan();
      board = await backend.board();
      await loadKnown();
      if (!pickerChecked) {
        pickerChecked = true;
        startupPick();
      }
      if (!digestChecked) {
        // Weekly digest (LATER-004): once per app start, after the first scan saved fresh snapshots.
        digestChecked = true;
        backend.digestIfDue().catch(() => {});
      }
      return report.projects.length;
    } catch (e) {
      error = String(e);
      return null;
    } finally {
      scanning = false;
    }
  }

  let confirmRestore = $state(false);
  async function restorePlacement() {
    confirmRestore = false;
    error = "";
    try {
      board = await backend.restorePlacement();
    } catch (e) {
      error = String(e);
    }
  }

  async function move(id: number, column: BoardColumn, before: number | null) {
    error = "";
    try {
      board = await backend.moveCard(id, column, before);
    } catch (e) {
      error = String(e);
    }
  }

  let selectedPath = $state<string | null>(null);
  const selected = $derived(projects.find((p) => p.project.path === selectedPath) ?? null);
  const selectedRow = $derived(board.find((r) => r.path === selectedPath) ?? null);

  async function saveNote(id: number, note: string) {
    try {
      board = await backend.setParkingNote(id, note.trim() ? note : null);
    } catch (e) {
      error = String(e);
    }
  }

  async function savePrivate(id: number, priv: boolean) {
    try {
      board = await backend.setPrivate(id, priv);
    } catch (e) {
      error = String(e);
    }
  }

  function open(p: ProjectReport) {
    selectedPath = p.project.path;
  }

  $effect(() => {
    rescan();
  });

  // LATER-003: in tray mode, watched changes and the tray's "Rescan now" trigger a zero-token rescan.
  $effect(() => {
    let stop: (() => void) | null = null;
    backend
      .onBackgroundRescan(() => {
        if (!scanning) rescan();
      })
      .then((s) => (stop = s))
      .catch(() => {});
    return () => stop?.();
  });
</script>

<div class="app">
<aside class="side" class:collapsed style:width={collapsed ? undefined : `${sideWidth}px`}>
  <div class="brand">
    <span class="logo" aria-hidden="true">◈</span>
    <h1 class="label">Logpose</h1>
    <button class="collapse" onclick={toggleCollapsed} aria-expanded={!collapsed} aria-label={collapsed ? "Expand the sidebar" : "Collapse the sidebar"} title={collapsed ? "Expand the sidebar" : "Collapse the sidebar"}>
      {@render icon("panel")}
    </button>
  </div>

  <nav aria-label="Views">
    {#each [["board", "Board"], ["list", "List"], ["stats", "Stats"], ["settings", "Settings"]] as [v, label] (v)}
      <button class="item" aria-current={view === v ? "page" : undefined} onclick={() => setView(v as View)} title={collapsed ? label : undefined}>
        {@render icon(v)}<span class="label">{label}</span>
      </button>
    {/each}
  </nav>

  {#if report}
    <section class="group" aria-labelledby="status-title">
      <h2 id="status-title" class="group-title label">Status</h2>
      <button
        class="item filter"
        aria-pressed={!hidden.length}
        onclick={() => (hidden = [])}
        title={collapsed ? `All ${projects.length} projects` : "Show every status"}
      >
        <span class="swatch all" aria-hidden="true"></span><span class="label grow">All projects</span><span class="num">{projects.length}</span>
      </button>
      {#each buckets as b (b)}
        <button
          class="item filter"
          aria-pressed={!hidden.includes(b)}
          onclick={() => toggleBucket(b)}
          title={`${hidden.includes(b) ? "Show" : "Hide"} ${bucketText[b].toLowerCase()} projects`}
        >
          <span class={`swatch dot ${b}`} aria-hidden="true"></span><span class="label grow">{bucketText[b]}</span><span class="num">{counts[b] ?? 0}</span>
        </button>
      {/each}
    </section>
  {/if}

  <section class="group card label" aria-labelledby="summaries-title">
    <h2 id="summaries-title" class="group-title">Summaries</h2>
    <div class="agents" role="radiogroup" aria-label="Agent for summaries">
      {#each [["claude-code", "Claude Code"], ["codex", "Codex"]] as [a, label] (a)}
        <button role="radio" aria-checked={agent === a} disabled={summarizing} onclick={() => setAgent(a as Agent)}>{label}</button>
      {/each}
    </div>
    <button
      class="primary wide"
      onclick={() => summarize(null, false)}
      disabled={summarizing || scanning || !report}
      title="Summarize every project with the chosen agent. Unchanged projects come from the cache and cost nothing."
    >
      {@render icon("sparkle")}{summarizing ? "Summarizing…" : "Update summaries"}
    </button>
  </section>
  {#if collapsed}
    <button class="item" onclick={() => summarize(null, false)} disabled={summarizing || scanning || !report} title={`Update summaries with ${agent === "codex" ? "Codex" : "Claude Code"}`}>
      {@render icon("sparkle")}
    </button>
  {/if}

  <div class="foot">
    <button class="item" onclick={rescan} disabled={scanning} title="Read git and agent logs again. Never calls an agent.">
      {@render icon("rescan")}<span class="label grow">{scanning ? "Scanning…" : "Rescan"}</span>
      {#if report && !scanning}<span class="num label">{(report.duration.secs + report.duration.nanos / 1e9).toFixed(1)} s</span>{/if}
    </button>
    <button
      class="item"
      onclick={() => setTheme(shownTheme() === "dark" ? "light" : "dark")}
      title={shownTheme() === "dark" ? "Switch to light theme" : "Switch to dark theme"}
    >
      {@render icon(shownTheme() === "dark" ? "sun" : "moon")}<span class="label">{shownTheme() === "dark" ? "Light theme" : "Dark theme"}</span>
    </button>
  </div>
  {#if !collapsed}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions (a separator with keyboard resizing, as in the ARIA window splitter pattern) -->
    <div
      class="resize"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the sidebar"
      aria-valuenow={sideWidth}
      aria-valuemin={minWidth}
      aria-valuemax={maxWidth}
      tabindex="0"
      onpointerdown={startResize}
      onkeydown={resizeKey}
    ></div>
  {/if}
</aside>

<main>
  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
  {#if synthNote}
    <p class="note muted" role="status">{synthNote}</p>
  {/if}

  {#if !report && scanning && view !== "settings"}
    <p class="muted pad">Scanning your projects… (no tokens are spent)</p>
  {:else if view === "board"}
    {@render tools(true)}
    {#if filtering && !shown.length}<p class="muted pad">No projects match the search and status filter.</p>{/if}
    <Board projects={shown} {board} {synth} onMove={move} onOpen={open} onRefresh={refresh} />
  {:else if view === "settings"}
    <Settings {projects} onSaved={rescan} />
  {:else if view === "list"}
    {@render tools(false)}
    {#if filtering && !shown.length}<p class="muted pad">No projects match the search and status filter.</p>{/if}
    <ProjectList projects={shown} {board} {synth} onOpen={open} />
  {:else}
    <Stats {projects} scannedAt={report?.taken_at ?? null} />
  {/if}
</main>
</div>

<svelte:window onkeydown={globalKey} />

{#snippet icon(name: string)}
  <svg class="ico" viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
    {#if name === "board"}<rect x="3" y="4" width="5" height="16" rx="1.5"/><rect x="10" y="4" width="5" height="11" rx="1.5"/><rect x="17" y="4" width="4" height="7" rx="1.5"/>
    {:else if name === "list"}<path d="M8 6h13M8 12h13M8 18h13"/><circle cx="4" cy="6" r="1"/><circle cx="4" cy="12" r="1"/><circle cx="4" cy="18" r="1"/>
    {:else if name === "stats"}<path d="M4 20V10M10 20V4M16 20v-7M22 20H2"/>
    {:else if name === "settings"}<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z"/>
    {:else if name === "sparkle"}<path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9L12 3Z"/><path d="M19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8L19 15Z"/>
    {:else if name === "rescan"}<path d="M21 12a9 9 0 1 1-2.6-6.4M21 4v5h-5"/>
    {:else if name === "sun"}<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>
    {:else if name === "moon"}<path d="M20 14.5A8.5 8.5 0 0 1 9.5 4a8.5 8.5 0 1 0 10.5 10.5Z"/>
    {:else if name === "panel"}<rect x="3" y="4" width="18" height="16" rx="2.5"/><path d="M9 4v16"/>
    {/if}
  </svg>
{/snippet}


{#snippet tools(onBoard: boolean)}
  <div class="tools">
    <input
      class="search"
      type="search"
      placeholder="Search projects  /"
      aria-label="Search projects by name, path or summary"
      bind:value={query}
      bind:this={searchInput}
      onkeydown={(e) => e.key === "Escape" && ((query = ""), e.currentTarget.blur())}
    />
    {#if filtering}<span class="muted">{shown.length} of {projects.length}</span>{/if}
    {#if onBoard}
      <span class="spacer"></span>
      {#if confirmRestore}
        <span>Move every card to its suggested column? Your manual placement will be replaced.</span>
        <button class="primary" onclick={restorePlacement}>Move cards</button>
        <button onclick={() => (confirmRestore = false)}>Cancel</button>
      {:else}
        <button
          class="link"
          onclick={() => (confirmRestore = true)}
          title="Now: the 3 most recent fresh projects · Next: fresh and cooling · Paused: stale · Maintenance: dormant and clean · Archived: other dormant"
        >
          Restore suggested placement
        </button>
      {/if}
    {/if}
  </div>
{/snippet}


{#if picker}
  <Picker info={picker} onPick={pick} onSkip={() => (picker = null)} />
{/if}

{#if selected}
  <Detail
    project={selected}
    synth={synth[selected.project.path] ?? null}
    row={selectedRow}
    onNote={saveNote}
    onPrivate={savePrivate}
    onRefresh={() => refresh(selected.project.path)}
    onClose={() => (selectedPath = null)}
    onRemoved={() => {
      selectedPath = null;
      rescan();
    }}
  />
{/if}

<style>
  /* UI-004: the window never scrolls; the main area does, and the board fills its height so its
     horizontal scrollbar stays at the bottom edge of the window. */
  .app {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    height: 100vh;
  }
  main {
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
  }
  /* Sidebar: views, the status filter, summaries, then rescan and theme. Resizable; collapses to a rail. */
  .side {
    position: sticky;
    top: 0;
    height: 100vh;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
    padding: 1.1rem 0.9rem 1rem;
    background: var(--surface);
    border-right: 1px solid var(--border);
    overflow-y: auto;
    overflow-x: hidden;
    z-index: 2;
    font-size: 0.95rem;
  }
  .side.collapsed {
    width: 4rem;
    padding-inline: 0.6rem;
    align-items: center;
  }
  .side.collapsed .label {
    display: none;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.65rem;
    padding-left: 0.3rem;
  }
  .collapsed .brand {
    flex-direction: column;
    padding: 0;
  }
  .logo {
    display: inline-grid;
    place-items: center;
    width: 2.1rem;
    height: 2.1rem;
    flex: none;
    border-radius: 9px;
    background: var(--accent);
    color: #fff;
    font-size: 1.15rem;
  }
  h1 {
    margin: 0;
    font-size: 1.2rem;
    letter-spacing: -0.01em;
    flex: 1;
  }
  .collapse {
    display: inline-grid;
    place-items: center;
    border: none;
    background: none;
    color: var(--muted);
    padding: 0.4rem;
    border-radius: 7px;
  }
  .collapse:hover {
    color: var(--fg);
    background: var(--gray-bg);
  }
  nav,
  .group,
  .foot {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    width: 100%;
  }
  .group-title {
    margin: 0;
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--muted);
    padding: 0 0.65rem 0.4rem;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    width: 100%;
    min-height: 2.4rem;
    border: none;
    background: none;
    color: var(--muted);
    padding: 0.5rem 0.65rem;
    border-radius: 8px;
    text-align: left;
    white-space: nowrap;
  }
  .collapsed .item {
    justify-content: center;
    padding-inline: 0;
  }
  .item:hover:not(:disabled) {
    color: var(--fg);
    background: var(--gray-bg);
  }
  .item[aria-current="page"] {
    color: var(--fg);
    background: var(--bg);
    box-shadow: var(--shadow);
    font-weight: 600;
  }
  .ico {
    flex: none;
  }
  .grow {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .num {
    font-variant-numeric: tabular-nums;
    font-size: 0.85rem;
  }
  /* Status filter rows: lit when shown, dimmed when filtered out. */
  .filter {
    min-height: 2.1rem;
    padding-block: 0.35rem;
  }
  .filter[aria-pressed="true"] {
    color: var(--fg);
  }
  .filter[aria-pressed="false"] {
    opacity: 0.45;
  }
  .filter[aria-pressed="false"] .swatch {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--muted);
  }
  .swatch {
    width: 0.7rem;
    height: 0.7rem;
    margin: 0 0.25rem;
    flex: none;
    border-radius: 50%;
  }
  .swatch.all {
    background: conic-gradient(var(--green) 0 25%, var(--blue) 0 50%, var(--amber) 0 75%, var(--muted) 0);
  }
  .card {
    gap: 0.6rem;
    padding: 0.85rem 0.8rem 0.9rem;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-sizing: border-box;
  }
  .card .group-title {
    padding: 0;
  }
  .agents {
    display: grid;
    grid-template-columns: 1fr 1fr;
    padding: 3px;
    border-radius: 8px;
    background: var(--gray-bg);
  }
  .agents button {
    border: none;
    background: none;
    color: var(--muted);
    padding: 0.4rem 0.3rem;
    border-radius: 6px;
    font-size: 0.85rem;
    white-space: nowrap;
  }
  .agents button[aria-checked="true"] {
    background: var(--bg);
    color: var(--fg);
    font-weight: 600;
    box-shadow: var(--shadow);
  }
  .wide {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    width: 100%;
    padding: 0.55rem 0.8rem;
    border-radius: 8px;
  }
  .foot {
    margin-top: auto;
  }
  .resize {
    position: absolute;
    top: 0;
    right: -3px;
    width: 6px;
    height: 100%;
    cursor: col-resize;
    touch-action: none;
  }
  .resize:hover,
  .resize:focus-visible {
    background: var(--accent);
    outline: none;
  }
  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.6rem;
    padding: 0.75rem 1.5rem 0;
    font-size: 0.85rem;
  }
  .search {
    font: inherit;
    width: min(20rem, 100%);
    padding: 0.4rem 0.7rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--fg);
  }
  .search:focus {
    background: var(--bg);
  }
  .spacer {
    flex: 1;
  }
  .note {
    font-size: 0.8rem;
    padding: 0.4rem 1.5rem 0;
    margin: 0;
  }
  .pad {
    padding: 1rem 1.5rem;
  }
</style>
