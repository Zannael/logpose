<script lang="ts">
  // UI-001: read-only list of every project with staleness, mess, VCS and agent badges. UI-002: the
  // whole row opens the detail, and each row shows its board column and summary.
  import { agentText, bucketText, columnText, days, messText, name } from "./labels";
  import type { BoardRow, ProjectReport, SynthState } from "./types";

  let {
    projects,
    board,
    synth,
    onOpen,
  }: {
    projects: ProjectReport[];
    board: BoardRow[];
    synth: Record<string, SynthState>;
    onOpen: (p: ProjectReport) => void;
  } = $props();

  const columnOf = $derived(new Map(board.map((r) => [r.path, r.column])));

  const sorted = $derived(
    [...projects].sort((a, b) => (a.staleness?.days ?? Infinity) - (b.staleness?.days ?? Infinity)),
  );
</script>

<table>
  <thead>
    <tr>
      <th scope="col">Project</th>
      <th scope="col" class="num">Last activity</th>
      <th scope="col">Column</th>
      <th scope="col">Staleness</th>
      <th scope="col">State</th>
      <th scope="col">Agents</th>
    </tr>
  </thead>
  <tbody>
    {#each sorted as p (p.project.path)}
      {@const col = columnOf.get(p.project.path)}
      {@const desc = synth[p.project.path]?.synthesis?.description}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions (the name button is the keyboard target) -->
      <tr onclick={(e) => !(e.target as HTMLElement).closest("button") && onOpen(p)}>
        <th scope="row">
          <button class="link name" onclick={() => onOpen(p)}>{name(p)}</button>
          {#if p.project.alias}<span class="muted small">{p.project.title}</span>{/if}
          {#if desc}<span class="desc" title={desc}>{desc}</span>{/if}
          <span class="path" title={p.project.path}>{p.project.path}</span>
        </th>
        <td class="num">{days(p)}</td>
        <td>{#if col}<span class={`col-dot ${col}`}></span>{columnText[col]}{/if}</td>
        <td>
          {#if p.staleness}<span class={`badge ${p.staleness.bucket}`}>{bucketText[p.staleness.bucket]}</span>{/if}
        </td>
        <td>
          {#if p.mess}
            <span class={`badge ${p.mess.label}`} title={`Mess score ${Math.round(p.mess.score)}/100`}>
              {messText[p.mess.label]} · {Math.round(p.mess.score)}
            </span>
          {:else}
            <span class="badge neutral">No VCS</span>
          {/if}
          {#if p.project.from_logs}<span class="badge neutral" title="Found only through agent session logs">From logs</span>{/if}
          {#if p.project.sub_repos.length}<span class="badge neutral">{p.project.sub_repos.length} sub-repo{p.project.sub_repos.length > 1 ? "s" : ""}</span>{/if}
        </td>
        <td>
          {#each p.activity?.agents ?? [] as a (a)}<span class={`chip ${a}`}>{agentText[a]}</span>{/each}
        </td>
      </tr>
    {/each}
  </tbody>
</table>

<style>
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.9rem;
  }
  th,
  td {
    text-align: left;
    padding: 0.55rem 1.5rem 0.55rem 0;
    border-bottom: 1px solid var(--border);
    vertical-align: middle;
  }
  th:first-child,
  td:first-child {
    padding-left: 1.5rem;
  }
  thead th {
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
    font-weight: 600;
  }
  tbody th {
    font-weight: normal;
  }
  tbody tr {
    cursor: pointer;
  }
  tbody tr:hover {
    background: var(--surface);
  }
  .desc {
    display: block;
    font-size: 0.8rem;
    max-width: 36rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col-dot {
    display: inline-block;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    margin-right: 0.4rem;
    background: var(--muted);
  }
  .col-dot.now {
    background: var(--green);
  }
  .col-dot.next {
    background: var(--blue);
  }
  .col-dot.paused {
    background: var(--amber);
  }
  .col-dot.maintenance {
    background: var(--violet);
  }
  .name {
    font-weight: 600;
    color: var(--fg);
    padding: 0;
  }
  .name:hover {
    color: var(--accent);
    text-decoration: underline;
  }
  .path {
    display: block;
    color: var(--muted);
    margin-top: 0.1rem;
    font-size: 0.75rem;
    max-width: 28rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .small {
    font-size: 0.75rem;
    margin-left: 0.3rem;
  }
</style>
