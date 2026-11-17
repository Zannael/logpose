<script lang="ts" module>
  import type { StatsView as View } from "./types";
  // PERF-002: results are kept per interval and project set for the life of the app (module scope,
  // so leaving the tab keeps them). A kept result is shown at once; it is recomputed in the
  // background when a newer scan exists, and on Refresh. Preset keys include today's date, so
  // results never outlive the day they were computed for.
  type Kept = { view: View; computedAt: number; scan: string | null };
  const kept = new Map<string, Kept>();
</script>

<script lang="ts">
  // STATS-004: retroactive stats from git history (STATS-001), session logs (STATS-002) and the
  // store's snapshots and board (STATS-003). Colors: validated with the dataviz palette checks
  // (agent pair = categorical slots 1–2; buckets = aqua/blue/yellow/violet; heatmap = one blue ramp).
  import { backend } from "./backend";
  import { bucketText, name } from "./labels";
  import StackedBars from "./StackedBars.svelte";
  import type { AgentSplit, Bucket, ProjectReport, StatsView } from "./types";

  let { projects, scannedAt }: { projects: ProjectReport[]; scannedAt: string | null } = $props();


  // UI-002: preset ranges ending today, or a custom interval (both days inclusive). The choice is
  // remembered on this machine.
  const presets = [
    ["1w", "1 week", 7],
    ["2w", "2 weeks", 14],
    ["1m", "1 month", 30],
    ["3m", "3 months", 91],
    ["6m", "6 months", 182],
    ["12m", "12 months", 365],
  ] as const;
  type Range = (typeof presets)[number][0] | "custom";
  const rangeKey = "logpose-stats-range";

  // Local calendar helpers (backend keys are local YYYY-MM-DD).
  const pad2 = (n: number) => String(n).padStart(2, "0");
  const key = (d: Date) => `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
  const parse = (k: string) => new Date(`${k}T00:00:00`);
  const short = (k: string) => parse(k).toLocaleDateString(undefined, { day: "numeric", month: "short" });
  const monthName = (k: string) => parse(k).toLocaleDateString(undefined, { month: "long", year: "numeric" });
  const daysBefore = (n: number) => {
    const d = new Date();
    d.setDate(d.getDate() - n);
    return key(d);
  };
  const todayKey = key(new Date());

  function loadRange(): { range: Range; from: string; to: string } {
    const fallback = { range: "12m" as Range, from: daysBefore(365), to: todayKey };
    try {
      const v = JSON.parse(localStorage.getItem(rangeKey) ?? "null");
      if (v?.range === "custom" && /^\d{4}-\d\d-\d\d$/.test(v.from) && /^\d{4}-\d\d-\d\d$/.test(v.to)) return v;
      if (presets.some(([k]) => k === v?.range)) return { ...fallback, range: v.range };
    } catch {
      // Remembering the range is a convenience only.
    }
    return fallback;
  }
  const saved = loadRange();
  let range = $state<Range>(saved.range);
  let customFrom = $state(saved.from);
  let customTo = $state(saved.to);

  /** The interval shown, as local YYYY-MM-DD days (inclusive). */
  const interval = $derived.by(() => {
    if (range === "custom") return { from: customFrom, to: customTo };
    const n = presets.find(([k]) => k === range)![2];
    return { from: daysBefore(n - 1), to: todayKey };
  });
  const invalid = $derived(!interval.from || !interval.to || interval.from > interval.to);

  $effect(() => {
    try {
      localStorage.setItem(rangeKey, JSON.stringify({ range, from: customFrom, to: customTo }));
    } catch {
      // See loadRange.
    }
  });

  let view = $state<StatsView | null>(null);
  let computedAt = $state<number | null>(null);
  let error = $state("");
  let loading = $state(false);
  let refreshTick = $state(0);
  let now = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(t);
  });

  const input = $derived(
    projects.map((p) => ({
      path: p.project.path,
      repos: [p.repo, ...p.sub_repos].filter((r) => r?.signals).map((r) => r!.path),
    })),
  );
  const cacheKey = $derived(`${interval.from}|${interval.to}|${input.map((p) => p.path).join(",")}`);

  let lastForced = 0;
  $effect(() => {
    const key = cacheKey;
    const { from, to } = interval;
    const forced = refreshTick !== lastForced;
    lastForced = refreshTick;
    if (invalid) return;
    const hit = kept.get(key);
    if (hit) {
      view = hit.view;
      computedAt = hit.computedAt;
      if (!forced && hit.scan === scannedAt) return;
    }
    loading = true;
    error = "";
    const scan = scannedAt;
    backend
      .stats(input, from, to === todayKey ? null : to)
      .then((v) => {
        const entry = { view: v, computedAt: Date.now(), scan };
        kept.set(key, entry);
        if (key === cacheKey) ({ view, computedAt } = entry);
      })
      .catch((e) => (error = String(e)))
      .finally(() => (loading = false));
  });

  const age = $derived.by(() => {
    if (computedAt == null) return "";
    const min = Math.round((now - computedAt) / 60_000);
    return min < 1 ? "just now" : min < 60 ? `${min} min ago` : new Date(computedAt).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  });

  /** Heatmap weeks (Monday first), oldest left, covering the selected range. */
  const heat = $derived.by(() => {
    const per = view?.git.commits_per_day ?? {};
    const first = parse(interval.from);
    const last = parse(interval.to);
    const start = new Date(first);
    start.setDate(start.getDate() - ((start.getDay() + 6) % 7));
    const weeks: { key: string; n: number; future: boolean }[][] = [];
    for (const d = new Date(start); d <= last || d.getDay() !== 1; d.setDate(d.getDate() + 1)) {
      if ((d.getDay() + 6) % 7 === 0) weeks.push([]);
      // Days outside the interval are drawn like future days (no data).
      weeks[weeks.length - 1].push({ key: key(d), n: per[key(d)] ?? 0, future: d > last || d < first });
    }
    return weeks;
  });
  const level = (n: number) => (n === 0 ? 0 : n <= 2 ? 1 : n <= 5 ? 2 : n <= 10 ? 3 : 4);
  let cell = $state<{ key: string; n: number; x: number; y: number } | null>(null);

  const totalCommits = $derived(Object.values(view?.git.commits_per_day ?? {}).reduce((a, b) => a + b, 0));
  const activeDays = $derived(Object.keys(view?.git.commits_per_day ?? {}).length);
  const sum = (f: (s: AgentSplit) => number) =>
    Object.values(view?.sessions.per_week ?? {}).reduce((a, s) => a + f(s), 0);

  const agentSeries = [
    { label: "Claude Code", color: "var(--s-claude)" },
    { label: "Codex", color: "var(--s-codex)" },
  ];
  const weekRows = (keys: string[], f: (k: string) => number[]) =>
    keys.sort().map((k) => ({ label: short(k), tip: `Week of ${short(k)}`, values: f(k) }));

  const touchedRows = $derived(
    weekRows(Object.keys(view?.git.projects_per_week ?? {}), (k) => [view!.git.projects_per_week[k]]),
  );
  const agentWeekRows = $derived(
    weekRows(Object.keys(view?.sessions.per_week ?? {}), (k) => {
      const s = view!.sessions.per_week[k];
      return [s.claude_code.sessions, s.codex.sessions];
    }),
  );
  const bucketKeys: Bucket[] = ["fresh", "cooling", "stale", "dormant"];
  const bucketSeries = bucketKeys.map((b) => ({ label: bucketText[b], color: `var(--b-${b})` }));
  const bucketRows = $derived(
    Object.keys(view?.buckets ?? {})
      .sort()
      .map((k) => ({ label: short(k), values: bucketKeys.map((b) => view!.buckets[k][b]) })),
  );

  const byPath = $derived(new Map(projects.map((p) => [p.project.path, p])));
  const projectRows = $derived(
    Object.entries(view?.sessions.per_project ?? {})
      .map(([path, s]) => ({
        name: byPath.get(path) ? name(byPath.get(path)!) : path.split("/").pop()!,
        path,
        c: s.claude_code.sessions,
        x: s.codex.sessions,
      }))
      .sort((a, b) => b.c + b.x - (a.c + a.x))
      .slice(0, 12),
  );
  const projectMax = $derived(Math.max(1, ...projectRows.map((r) => r.c + r.x)));
  const moves = $derived(Object.entries(view?.moves ?? {}).sort(([a], [b]) => b.localeCompare(a)));
</script>

<div class="stats">
  <div class="filters">
    <div class="seg" role="radiogroup" aria-label="Range">
      {#each presets as [k, label] (k)}
        <button role="radio" aria-checked={range === k} onclick={() => (range = k)}>{label}</button>
      {/each}
      <button
        role="radio"
        aria-checked={range === "custom"}
        onclick={() => {
          if (range !== "custom") ({ from: customFrom, to: customTo } = interval);
          range = "custom";
        }}>Custom…</button
      >
    </div>
    {#if range === "custom"}
      <label>From <input type="date" bind:value={customFrom} max={todayKey} /></label>
      <label>to <input type="date" bind:value={customTo} max={todayKey} /></label>
      {#if invalid}<span class="error">The start must be on or before the end.</span>{/if}
    {:else}
      <span class="muted">{short(interval.from)} – today</span>
    {/if}
    <span class="spacer"></span>
    <span class="muted" role="status" title={computedAt ? `Computed ${new Date(computedAt).toLocaleString()}` : ""}>
      {#if loading}{view ? "Updating…" : "Loading…"}{:else if age}Updated {age}{/if}
    </span>
    <button onclick={() => refreshTick++} disabled={loading} title="Read git history, agent logs and snapshots again">Refresh</button>
  </div>

  {#if error}<p class="error" role="alert">{error}</p>{/if}

  {#if view}
    <div class="tiles">
      <div class="tile"><span class="big">{totalCommits}</span><span class="muted">your commits</span></div>
      <div class="tile"><span class="big">{activeDays}</span><span class="muted">days with commits</span></div>
      <div class="tile">
        <span class="big">{sum((s) => s.claude_code.sessions)}</span><span class="muted">Claude Code sessions</span>
      </div>
      <div class="tile"><span class="big">{sum((s) => s.codex.sessions)}</span><span class="muted">Codex sessions</span></div>
    </div>

    <section class="card">
      <h2>Commits per day</h2>
      <p class="muted note">
        Only your commits ({view.git.emails.join(", ") || "no git user.email found"}), across all projects and sub-repos.
      </p>
      <div class="heat-wrap">
        <div class="heat" role="img" aria-label={`Commit heatmap: ${totalCommits} commits on ${activeDays} days`}>
          {#each heat as week, w (w)}
            <div class="week">
              {#each week as d (d.key)}
                <span
                  class={`day l${level(d.n)}`}
                  class:future={d.future}
                  role="presentation"
                  onmouseenter={(e) => {
                    const r = e.currentTarget.getBoundingClientRect();
                    const box = e.currentTarget.closest(".heat-wrap")!.getBoundingClientRect();
                    cell = { key: d.key, n: d.n, x: r.left - box.left + r.width / 2, y: r.top - box.top };
                  }}
                  onmouseleave={() => (cell = null)}
                ></span>
              {/each}
            </div>
          {/each}
        </div>
        {#if cell}
          <div class="tip" style:left={`${cell.x}px`} style:top={`${cell.y - 34}px`}>
            <strong>{cell.n}</strong> commit{cell.n === 1 ? "" : "s"} · {short(cell.key)}
          </div>
        {/if}
      </div>
      <div class="scale muted">
        Less {#each [0, 1, 2, 3, 4] as l (l)}<span class={`day l${l}`}></span>{/each} More
        <span class="steps">(0 · 1–2 · 3–5 · 6–10 · 11+)</span>
      </div>
      {#if view.git.errors.length}
        <p class="error">{view.git.errors.length} repositories could not be read: {view.git.errors.join("; ")}</p>
      {/if}
    </section>

    <div class="grid2">
      <section class="card">
        <h2>Projects touched per week</h2>
        <StackedBars
          caption="Projects with at least one of your commits, per week"
          series={[{ label: "Projects", color: "var(--s-claude)" }]}
          rows={touchedRows}
        />
      </section>
      <section class="card">
        <h2>Agent sessions per week</h2>
        <StackedBars caption="Claude Code and Codex sessions per week" series={agentSeries} rows={agentWeekRows} />
      </section>
    </div>

    <div class="grid2">
      <section class="card">
        <h2>Agent split by project</h2>
        <ul class="legend">
          {#each agentSeries as s (s.label)}<li><span class="key" style:background={s.color}></span>{s.label}</li>{/each}
        </ul>
        <ol class="hbars">
          {#each projectRows as r (r.path)}
            <li title={r.path}>
              <span class="pname">{r.name}</span>
              <span class="track">
                {#if r.c}<span class="seg" style:width={`${(r.c / projectMax) * 100}%`} style:background="var(--s-claude)"></span>{/if}
                {#if r.x}<span class="seg" style:width={`${(r.x / projectMax) * 100}%`} style:background="var(--s-codex)"></span>{/if}
              </span>
              <span class="num muted">{r.c} · {r.x}</span>
            </li>
          {/each}
        </ol>
        {#if !projectRows.length}<p class="muted">No agent sessions in this range.</p>{/if}
        <p class="muted note">
          Sessions per project (Claude Code · Codex), top 12. Outside every project:
          {view.sessions.unattributed.claude_code.sessions} · {view.sessions.unattributed.codex.sessions}.
        </p>
      </section>
      <section class="card">
        <h2>Projects by staleness</h2>
        {#if bucketRows.length}
          <StackedBars caption="Projects per staleness bucket on each scan day" series={bucketSeries} rows={bucketRows} />
          <p class="muted note">One bar per day with a scan (Logpose has no background service).</p>
        {:else}
          <p class="muted">No scans saved in this range yet.</p>
        {/if}
      </section>
    </div>

    <section class="card">
      <h2>Archived and finished per month</h2>
      {#if moves.length}
        <table class="moves">
          <thead><tr><th scope="col">Month</th><th scope="col">Archived</th><th scope="col">Maintenance (finished)</th></tr></thead>
          <tbody>
            {#each moves as [m, v] (m)}
              <tr><th scope="row">{monthName(m)}</th><td>{v.archived}</td><td>{v.maintenance}</td></tr>
            {/each}
          </tbody>
        </table>
      {:else}
        <p class="muted">No project has been moved to Archived or Maintenance yet.</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .stats {
    --s-claude: #2a78d6;
    --s-codex: #eb6834;
    --b-fresh: #1baf7a;
    --b-cooling: #2a78d6;
    --b-stale: #eda100;
    --b-dormant: #4a3aa7;
    --h0: var(--gray-bg);
    --h1: #b7d3f6;
    --h2: #6da7ec;
    --h3: #2a78d6;
    --h4: #184f95;
    padding: 1rem 1.5rem 2rem;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root:not([data-theme="light"])) .stats {
      --s-claude: #3987e5;
      --s-codex: #d95926;
      --b-fresh: #199e70;
      --b-cooling: #3987e5;
      --b-stale: #c98500;
      --b-dormant: #9085e9;
      --h1: #184f95;
      --h2: #256abf;
      --h3: #3987e5;
      --h4: #86b6ef;
    }
  }
  :global(:root[data-theme="dark"]) .stats {
    --s-claude: #3987e5;
    --s-codex: #d95926;
    --b-fresh: #199e70;
    --b-cooling: #3987e5;
    --b-stale: #c98500;
    --b-dormant: #9085e9;
    --h1: #184f95;
    --h2: #256abf;
    --h3: #3987e5;
    --h4: #86b6ef;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 0.6rem 1rem;
    align-items: center;
    font-size: 0.85rem;
  }
  .spacer {
    flex: 1;
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
  }
  .seg button {
    border: none;
    border-radius: 0;
    padding: 0.35rem 0.7rem;
    font-size: 0.85rem;
    color: var(--muted);
    background: none;
  }
  .seg button + button {
    border-left: 1px solid var(--border);
  }
  .seg button[aria-checked="true"] {
    background: var(--gray-bg);
    color: var(--fg);
    font-weight: 600;
  }
  input[type="date"] {
    font: inherit;
    margin-left: 0.3rem;
    padding: 0.2rem 0.3rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
  }
  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
    gap: 0.75rem;
  }
  .tile {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.7rem 0.9rem;
    display: flex;
    flex-direction: column;
    font-size: 0.8rem;
  }
  .big {
    font-size: 1.6rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.9rem 1rem;
    min-width: 0;
  }
  .grid2 {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr));
    gap: 1rem;
  }
  h2 {
    font-size: 0.9rem;
    margin: 0 0 0.5rem;
  }
  .note {
    font-size: 0.75rem;
    margin: 0.3rem 0 0.5rem;
  }
  .heat-wrap {
    position: relative;
    overflow-x: auto;
    padding-top: 0.2rem;
  }
  .heat {
    display: flex;
    gap: 2px;
  }
  .week {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .day {
    display: inline-block;
    width: 11px;
    height: 11px;
    border-radius: 2px;
    background: var(--h0);
  }
  .day.l1 {
    background: var(--h1);
  }
  .day.l2 {
    background: var(--h2);
  }
  .day.l3 {
    background: var(--h3);
  }
  .day.l4 {
    background: var(--h4);
  }
  .day.future {
    visibility: hidden;
  }
  .scale {
    display: flex;
    align-items: center;
    gap: 3px;
    font-size: 0.72rem;
    margin-top: 0.4rem;
  }
  .steps {
    margin-left: 0.5rem;
  }
  .tip {
    position: absolute;
    transform: translateX(-50%);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.25rem 0.5rem;
    font-size: 0.75rem;
    white-space: nowrap;
    pointer-events: none;
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.12);
  }
  .legend {
    display: flex;
    gap: 1rem;
    list-style: none;
    margin: 0 0 0.5rem;
    padding: 0;
    font-size: 0.78rem;
    color: var(--muted);
  }
  .key {
    display: inline-block;
    width: 0.65rem;
    height: 0.65rem;
    border-radius: 2px;
    margin-right: 0.35rem;
    vertical-align: -1px;
  }
  .hbars {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    font-size: 0.8rem;
  }
  .hbars li {
    display: grid;
    grid-template-columns: 9rem 1fr 3.5rem;
    align-items: center;
    gap: 0.5rem;
  }
  .pname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .track {
    display: flex;
    gap: 2px;
    height: 10px;
  }
  .seg {
    height: 100%;
  }
  .seg:last-child {
    border-radius: 0 4px 4px 0;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .moves {
    border-collapse: collapse;
    font-size: 0.85rem;
  }
  .moves th,
  .moves td {
    text-align: left;
    padding: 0.25rem 1.5rem 0.25rem 0;
    border-bottom: 1px solid var(--border);
  }
  .moves td {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .moves thead th {
    color: var(--muted);
    font-weight: 600;
    font-size: 0.75rem;
  }
  .moves tbody th {
    font-weight: normal;
  }
  .error {
    color: var(--red);
  }
</style>
