<script lang="ts">
  // DETAIL-001: everything the scan knows about one project, without tokens: raw signals per
  // repository (own repo + sub-repos), aggregated mess inputs, recent commits and last prompts.
  import { backend } from "./backend";
  import { agentText, ago, bucketText, messText, name, relative, when } from "./labels";
  import RemoveDialog from "./RemoveDialog.svelte";
  import type { BoardRow, Commit, ProjectReport, RepoReport, RuleStatus, SynthState } from "./types";

  let {
    project: p,
    synth,
    row,
    onNote,
    onPrivate,
    onRefresh,
    onClose,
    onRemoved,
  }: {
    project: ProjectReport;
    synth: SynthState | null;
    row: BoardRow | null;
    onNote: (id: number, note: string) => void;
    onPrivate: (id: number, priv: boolean) => void;
    onRefresh: () => void;
    onClose: () => void;
    onRemoved: () => void;
  } = $props();

  // Per-project state resets when another project is opened. Keyed on the path string, not on `p`:
  // a background rescan hands over a new report object for the same project, and an effect reading
  // `p.project.path` directly would rerun and wipe the state (e.g. close the removal dialog).
  const path = $derived(p.project.path);

  // DEL-002: the removal dialog sits on top of the panel; Escape closes the dialog first.
  let removing = $state(false);
  $effect(() => {
    void path;
    removing = false;
  });

  // DETAIL-002: one-line parking note, saved on Enter or when the field loses focus.
  let note = $state("");
  $effect(() => {
    note = row?.parking_note ?? "";
  });
  let openStatus = $state("");
  $effect(() => {
    void path;
    openStatus = "";
  });
  async function openIt() {
    openStatus = "Opening…";
    try {
      const o = await backend.openProject(p.project.path);
      const app = o.program.split(/[\\/]/).pop();
      openStatus = o.fell_back ? `Opened in ${app} (the preferred editor was not found)` : `Opened in ${app}`;
    } catch (e) {
      openStatus = String(e);
    }
  }

  let launchStatus = $state("");
  $effect(() => {
    void path;
    launchStatus = "";
  });
  async function launch(agent: "claude-code" | "codex") {
    launchStatus = "Starting…";
    try {
      const term = await backend.launchAgent(p.project.path, agent);
      launchStatus = `Started ${agent === "codex" ? "Codex" : "Claude Code"} in ${term.split(/[\\/]/).pop()}`;
    } catch (e) {
      launchStatus = String(e);
    }
  }

  function commitNote() {
    if (row && note.trim() !== (row.parking_note ?? "")) onNote(row.id, note);
  }

  const sourceText = { cache: "saved summary", agent: "just summarized", "next-md": "from NEXT.md" } as const;

  type Commits = { list: Commit[] | null; error: string };

  const repos = $derived([...(p.repo ? [p.repo] : []), ...p.sub_repos]);
  let commits = $state<Record<string, Commits>>({});
  let closeButton = $state<HTMLButtonElement>();

  function repoLabel(r: RepoReport) {
    return r.path === p.project.path ? "This repo" : relative(r.path, p.project.path);
  }

  $effect(() => {
    const paths = repos.map((r) => r.path);
    commits = Object.fromEntries(paths.map((path) => [path, { list: null, error: "" }]));
    for (const path of paths) {
      const limit = path === p.project.path ? 10 : 5;
      backend
        .recentCommits(path, limit)
        .then((list) => (commits[path] = { list, error: "" }))
        .catch((e) => (commits[path] = { list: [], error: String(e) }));
    }
  });

  $effect(() => {
    closeButton?.focus();
  });

  // LATER-001: opt-in rule asking the agents to keep NEXT.md current.
  let rule = $state<RuleStatus | null>(null);
  let ruleError = $state("");
  let ruleBusy = $state(false);
  $effect(() => {
    const path = p.project.path;
    backend
      .agentRuleStatus(path)
      .then((r) => (rule = r))
      .catch((e) => (ruleError = String(e)));
  });

  async function toggleRule() {
    if (!rule) return;
    ruleBusy = true;
    ruleError = "";
    try {
      rule = await backend.setAgentRule(p.project.path, rule.files.length === 0, p.activity?.agents ?? []);
    } catch (e) {
      ruleError = String(e);
    } finally {
      ruleBusy = false;
    }
  }

  const yesNo = (b: boolean) => (b ? "yes" : "no");
  const num = (n: number | null | undefined, digits = 0) => (n == null ? "–" : n.toFixed(digits));
  const shortRef = (r: string | null) => r?.replace(/^refs\/remotes\//, "") ?? "none";
  const prompts = $derived([...(p.activity?.last_prompts ?? [])].reverse());
  const m = $derived(p.mess_inputs);
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && !removing && onClose()} />

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="panel" role="dialog" aria-modal="true" aria-labelledby="detail-title">
  <header>
    <div>
      <h2 id="detail-title">{name(p)}</h2>
      {#if p.project.alias}<span class="muted small">{p.project.title}</span>{/if}
      <p class="path">{p.project.path}</p>
      <div>
        {#if p.staleness}<span class={`badge ${p.staleness.bucket}`}>{bucketText[p.staleness.bucket]}</span>{/if}
        {#if p.mess}
          <span class={`badge ${p.mess.label}`}>{messText[p.mess.label]} · {Math.round(p.mess.score)}</span>
        {:else}
          <span class="badge neutral">No VCS</span>
        {/if}
        {#if p.project.from_logs}<span class="badge neutral">From logs</span>{/if}
        {#each p.activity?.agents ?? [] as a (a)}<span class="chip">{agentText[a]}</span>{/each}
      </div>
    </div>
    <div class="head-actions">
      <button class="primary" onclick={openIt} title="PyCharm for Python, WebStorm for other code, the file manager otherwise">Open</button>
      <button bind:this={closeButton} onclick={onClose} aria-label="Close details">✕</button>
      {#if openStatus}<span class="muted open-status" role="status">{openStatus}</span>{/if}
    </div>
  </header>

  <section>
    <h3>Summary</h3>
    {#if synth?.synthesis}
      {@const s = synth.synthesis}
      <p class="lead">{s.description}</p>
      <p class="muted small-note">
        {synth.source ? sourceText[synth.source] : ""}{#if synth.outdated} · <strong>outdated</strong>: the project changed since{/if}
        · confidence {s.confidence}
      </p>
      <h4>State</h4>
      <p>{s.state}</p>
      {#if s.last_goal}<h4>Last goal</h4><p>{s.last_goal}</p>{/if}
      <h4>Next steps</h4>
      <ol class="steps">{#each s.next_steps as step, i (i)}<li>{step}</li>{/each}</ol>
      {#if s.blockers.length}
        <h4>Blockers</h4>
        <ul class="steps">{#each s.blockers as b, i (i)}<li>{b}</li>{/each}</ul>
      {/if}
    {:else if !synth?.running}
      <p class="muted">No summary yet. It costs a few tokens with the agent chosen in the header.</p>
    {/if}
    {#if synth?.error}<p class="err">Summary failed: {synth.error}</p>{/if}
    <button onclick={onRefresh} disabled={synth?.running}>
      {synth?.running ? "Summarizing…" : synth?.synthesis ? "Refresh summary" : "Summarize"}
    </button>
    <div class="launch">
      <span class="muted">Continue in a terminal:</span>
      <button onclick={() => launch("claude-code")} title="Starts Claude Code in this folder with the summary as the first message">Claude Code</button>
      <button onclick={() => launch("codex")} title="Starts Codex in this folder with the summary as the first message">Codex</button>
      {#if launchStatus}<span class="muted" role="status">{launchStatus}</span>{/if}
    </div>
    {#if row}
      <label class="check private">
        <input type="checkbox" checked={row.private} onchange={(e) => onPrivate(row.id, e.currentTarget.checked)} />
        Keep my prompts and the agents' replies out of this project's summaries
      </label>
    {/if}
  </section>

  {#if row}
    <section>
      <h3><label for="parking-note">Parking note</label></h3>
      <input
        id="parking-note"
        class="note-input"
        maxlength="200"
        placeholder="Where you left off, e.g. waiting on the API key"
        bind:value={note}
        onblur={commitNote}
        onkeydown={(e) => e.key === "Enter" && (e.currentTarget as HTMLInputElement).blur()}
      />
      <p class="muted small-note">One line, saved on this computer only. It is included in the summary context.</p>
    </section>
  {/if}

  <section>
    <h3>Activity</h3>
    <table class="kv">
      <tbody>
        <tr>
          <th scope="row">Last activity</th>
          <td title={when(p.staleness?.last_activity)}>
            {ago(p.staleness?.last_activity)}{#if p.staleness}&nbsp;({num(p.staleness.days, 1)} days){/if}
          </td>
        </tr>
        <tr>
          <th scope="row">Latest file change</th>
          <td title={when(p.mtime.latest?.modified_at)}>
            {#if p.mtime.latest}
              <code>{relative(p.mtime.latest.path, p.project.path)}</code> · {ago(p.mtime.latest.modified_at)}
            {:else}–{/if}
          </td>
        </tr>
        <tr><th scope="row">Files seen</th><td>{p.mtime.files_seen}</td></tr>
        {#if p.activity}
          <tr>
            <th scope="row">Agent sessions</th>
            <td>{p.activity.sessions_7d} in 7 days · {p.activity.sessions_30d} in 30 days · {p.activity.sessions_total} total</td>
          </tr>
          <tr>
            <th scope="row">Last session</th>
            <td title={when(p.activity.last_session_at)}>
              {agentText[p.activity.last_agent]} · {ago(p.activity.last_session_at)}{#if p.activity.title}&nbsp;· “{p.activity.title}”{/if}
            </td>
          </tr>
        {:else}
          <tr><th scope="row">Agent sessions</th><td>none found</td></tr>
        {/if}
      </tbody>
    </table>
  </section>

  {#if !p.repo}
    <section>
      <h3>Git</h3>
      <p class="muted">
        {#if repos.length}
          No version control at the project root: staleness and the mess score come from its sub-repos, file times and
          agent sessions.
        {:else}
          No version control: staleness comes from file times and agent sessions, and there is no mess score.
        {/if}
      </p>
    </section>
  {/if}
  {#if repos.length}
    <section>
      <h3>Git signals</h3>
      <div class="scroll">
        <table class="grid">
          <thead>
            <tr>
              <th scope="col"></th>
              {#each repos as r (r.path)}<th scope="col" title={r.path}>{repoLabel(r)}</th>{/each}
            </tr>
          </thead>
          <tbody>
            {#snippet row(label: string, cell: (r: RepoReport) => string)}
              <tr>
                <th scope="row">{label}</th>
                {#each repos as r (r.path)}<td>{r.signals ? cell(r) : "–"}</td>{/each}
              </tr>
            {/snippet}
            {@render row("Branch", (r) => `${r.signals!.branch ?? "detached"}${r.signals!.on_default_branch ? "" : ` (default ${r.signals!.default_branch ?? "?"})`}`)}
            {@render row("Upstream", (r) => shortRef(r.signals!.upstream))}
            {@render row("Ahead / behind", (r) => `${r.signals!.ahead} / ${r.signals!.behind}`)}
            {@render row("Staged · modified · untracked", (r) => `${r.signals!.staged} · ${r.signals!.modified} · ${r.signals!.untracked}`)}
            {@render row("Conflicted", (r) => String(r.signals!.conflicted))}
            {@render row("Lines + / −", (r) => (r.dirty ? `+${r.dirty.lines_added} / −${r.dirty.lines_removed}` : "–"))}
            {@render row("Oldest uncommitted change", (r) => ago(r.dirty?.oldest_change_at))}
            {@render row("Stashes", (r) => `${r.signals!.stash_count}${r.signals!.oldest_stash_at ? ` (oldest ${ago(r.signals!.oldest_stash_at)})` : ""}`)}
            {@render row("Operation in progress", (r) => r.signals!.operation ?? "none")}
            {@render row("Unmerged branches", (r) => r.unmerged_branches.map((b) => b.name).join(", ") || "none")}
            {@render row("TODOs added (last 10 commits)", (r) => String(r.todos_added))}
            {@render row("Last commit", (r) => ago(r.signals!.last_commit_at))}
            {#if repos.some((r) => r.error)}
              <tr>
                <th scope="row">Error</th>
                {#each repos as r (r.path)}<td class="err">{r.error ?? ""}</td>{/each}
              </tr>
            {/if}
          </tbody>
        </table>
      </div>
      {#if m}
        <h4>Mess inputs {#if p.sub_repos.length}<span class="muted small">(sub-repos included)</span>{/if}</h4>
        <table class="kv">
          <tbody>
            <tr><th scope="row">Uncommitted files</th><td>{m.uncommitted_files}{#if m.oldest_uncommitted_days != null}, oldest {num(m.oldest_uncommitted_days)} days{/if}</td></tr>
            <tr><th scope="row">Unpushed commits</th><td>{m.unpushed_commits}</td></tr>
            <tr><th scope="row">Stashes</th><td>{m.stash_count}</td></tr>
            <tr><th scope="row">Stale branches</th><td>{m.stale_branches}</td></tr>
            <tr><th scope="row">No upstream</th><td>{yesNo(m.no_upstream)}</td></tr>
            <tr><th scope="row">Operation or conflict</th><td>{yesNo(m.operation_in_progress)}</td></tr>
          </tbody>
        </table>
      {/if}
    </section>

    <section>
      <h3>Recent commits</h3>
      {#each repos as r (r.path)}
        {@const c = commits[r.path]}
        {#if repos.length > 1}<h4 title={r.path}>{repoLabel(r)}</h4>{/if}
        {#if !c || c.list === null}
          <p class="muted">Loading…</p>
        {:else if c.error}
          <p class="err">{c.error}</p>
        {:else if !c.list.length}
          <p class="muted">No commits</p>
        {:else}
          <ol class="commits">
            {#each c.list as commit, i (i)}
              <li><span class="muted when" title={when(commit.at)}>{ago(commit.at)}</span> {commit.subject}</li>
            {/each}
          </ol>
        {/if}
      {/each}
    </section>
  {/if}

  <section>
    <h3>NEXT.md</h3>
    <label class="check">
      <input type="checkbox" checked={!!rule?.files.length} disabled={!rule || ruleBusy} onchange={toggleRule} />
      Ask coding agents to keep <code>NEXT.md</code> current
    </label>
    <p class="muted small-note">
      {#if rule?.files.length}
        Rule added to {rule.files.join(" and ")}. When <code>NEXT.md</code> is newer than the latest activity, Logpose reads
        it instead of calling an agent.
      {:else}
        Adds a short marked block to this project's {(p.activity?.agents ?? []).length ? "agent instruction files" : "AGENTS.md"}
        (CLAUDE.md for Claude Code, AGENTS.md for Codex). Turning it off removes the block and nothing else.
      {/if}
    </p>
    {#if ruleError}<p class="err">{ruleError}</p>{/if}
  </section>

  <section>
    <h3>Last prompts</h3>
    {#if prompts.length}
      <ol class="prompts">
        {#each prompts as pr, i (i)}
          <li><span class="muted when" title={when(pr.at)}>{ago(pr.at)}</span> {pr.text}</li>
        {/each}
      </ol>
      {#if p.activity?.final_message}
        <h4>Last agent message</h4>
        <blockquote>{p.activity.final_message}</blockquote>
      {/if}
    {:else}
      <p class="muted">No agent prompts found for this project.</p>
    {/if}
  </section>

  <section class="remove">
    <h3>Remove project</h3>
    <p class="muted small-note">
      Moves the folder to the Trash and cleans up what it left behind: Docker containers, volumes and images, and,
      if you choose, its agent session logs. You see everything, with sizes, before anything happens.
    </p>
    <button class="danger-outline" onclick={() => (removing = true)}>Remove project…</button>
  </section>
</div>

{#if removing}
  <RemoveDialog path={p.project.path} displayName={name(p)} onClose={() => (removing = false)} onRemoved={onRemoved} />
{/if}

<style>
  .remove {
    margin-top: 2rem;
    padding-top: 0.25rem;
    border-top: 1px solid var(--border);
  }
  .remove .small-note {
    margin-bottom: 0.75rem;
  }
  .danger-outline {
    color: var(--red);
    border-color: color-mix(in srgb, var(--red) 55%, var(--border));
  }
  .danger-outline:hover:not(:disabled) {
    background: var(--red-bg);
    border-color: var(--red);
  }
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.3);
    z-index: 10;
  }
  .panel {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: min(44rem, 100%);
    overflow-y: auto;
    background: var(--bg);
    border-left: 1px solid var(--border);
    z-index: 11;
    padding: 0 1.25rem 2rem;
    box-sizing: border-box;
    font-size: 0.9rem;
  }
  header {
    position: sticky;
    top: 0;
    background: var(--bg);
    display: flex;
    justify-content: space-between;
    align-items: start;
    gap: 1rem;
    padding: 1rem 0 0.75rem;
    border-bottom: 1px solid var(--border);
  }
  h2 {
    display: inline;
    margin: 0;
    font-size: 1.2rem;
  }
  h3 {
    font-size: 0.8rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    margin: 1.25rem 0 0.5rem;
  }
  h4 {
    font-size: 0.85rem;
    margin: 0.9rem 0 0.3rem;
  }
  .path {
    color: var(--muted);
    font-size: 0.78rem;
    margin: 0.3rem 0 0.5rem;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: 0.75rem;
    margin-left: 0.3rem;
    font-weight: normal;
  }
  table {
    border-collapse: collapse;
    width: 100%;
  }
  th,
  td {
    text-align: left;
    padding: 0.3rem 0.75rem 0.3rem 0;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
  tbody th {
    font-weight: normal;
    color: var(--muted);
    white-space: nowrap;
  }
  .kv th {
    width: 12rem;
  }
  .grid thead th {
    font-size: 0.78rem;
    white-space: nowrap;
  }
  .grid td {
    font-variant-numeric: tabular-nums;
  }
  .scroll {
    overflow-x: auto;
  }
  ol {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  ol li {
    padding: 0.4rem 0;
    border-bottom: 1px solid var(--border);
  }
  .when {
    display: inline-block;
    min-width: 4.5rem;
    font-size: 0.78rem;
  }
  blockquote {
    margin: 0;
    padding: 0.5rem 0.75rem;
    border-left: 3px solid var(--border);
    background: var(--gray-bg);
    border-radius: 0 6px 6px 0;
    white-space: pre-wrap;
  }
  .err {
    color: var(--red);
  }
  .head-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 0.4rem;
    max-width: 14rem;
  }
  .open-status {
    font-size: 0.75rem;
    text-align: right;
    width: 100%;
  }
  .launch {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
    margin-top: 0.6rem;
    font-size: 0.85rem;
  }
  .launch button {
    padding: 0.25rem 0.6rem;
  }
  .note-input {
    width: 100%;
    box-sizing: border-box;
    font: inherit;
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
  }
  .private {
    margin-top: 0.6rem;
    font-size: 0.85rem;
  }
  .lead {
    font-size: 1rem;
    margin: 0 0 0.2rem;
  }
  .steps {
    margin: 0 0 0.6rem;
    padding-left: 1.2rem;
    list-style: revert;
  }
  .steps li {
    border: none;
    padding: 0.2rem 0;
  }
  .check {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }
  .small-note {
    font-size: 0.8rem;
    margin: 0.3rem 0 0;
  }
</style>
