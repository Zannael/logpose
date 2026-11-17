<script lang="ts">
  // BOARD-001/002: Kanban columns (DEC-004). Column and order live in the SQLite store; only projects
  // present in the current scan are shown. Cards move by dragging the grip (svelte-dnd-action, pointer
  // and keyboard), with the ↑ ↓ buttons, or with the column menu.
  import { dragHandle, dragHandleZone, type DndEvent } from "svelte-dnd-action";
  import { flip } from "svelte/animate";
  import { agentText, bucketText, columnText, columns, days, messText, name, nowLimit } from "./labels";
  import type { BoardColumn, BoardRow, ProjectReport, SynthState } from "./types";

  let {
    projects,
    board,
    synth,
    onMove,
    onOpen,
    onRefresh,
  }: {
    projects: ProjectReport[];
    board: BoardRow[];
    synth: Record<string, SynthState>;
    onMove: (id: number, column: BoardColumn, before: number | null) => void;
    onOpen: (p: ProjectReport) => void;
    onRefresh: (path: string) => void;
  } = $props();

  type Card = { id: number; row: BoardRow; p: ProjectReport };
  const flipMs = 150;

  const byPath = $derived(new Map(projects.map((p) => [p.project.path, p])));
  const cards = $derived(
    Object.fromEntries(
      columns.map((c) => [
        c,
        board
          .filter((r) => r.column === c && byPath.has(r.path))
          .sort((a, b) => a.position - b.position)
          .map((row) => ({ id: row.id, row, p: byPath.get(row.path)! })),
      ]),
    ) as Record<BoardColumn, Card[]>,
  );

  // Working copy the drag library reorders while dragging; reset from the store's board otherwise.
  let zones = $state<Record<BoardColumn, Card[]>>({ now: [], next: [], paused: [], maintenance: [], archived: [] });
  $effect(() => {
    zones = cards;
  });

  function consider(c: BoardColumn, e: CustomEvent<DndEvent<Card>>) {
    zones[c] = e.detail.items;
  }

  function finalize(c: BoardColumn, e: CustomEvent<DndEvent<Card>>) {
    zones[c] = e.detail.items;
    // Finalize fires for the source and the target zone; only the target contains the dropped card.
    const id = Number(e.detail.info.id);
    const i = zones[c].findIndex((x) => x.id === id);
    if (i >= 0) onMove(id, c, zones[c][i + 1]?.id ?? null);
  }

  function up(c: BoardColumn, i: number) {
    onMove(cards[c][i].row.id, c, cards[c][i - 1].row.id);
  }

  function down(c: BoardColumn, i: number) {
    onMove(cards[c][i].row.id, c, cards[c][i + 2]?.row.id ?? null);
  }

  // UI-002: the whole card opens the detail (a bigger target than the title); its own controls don't.
  function cardClick(e: MouseEvent, p: ProjectReport) {
    if ((e.target as HTMLElement).closest("button, select, .grip")) return;
    onOpen(p);
  }
</script>

<div class="board">
  {#each columns as c (c)}
    {@const list = cards[c]}
    <section class="column" aria-labelledby={`col-${c}`} data-column={c}>
      <h2 id={`col-${c}`}><span class={`col-dot ${c}`}></span>{columnText[c]} <span class="count">{list.length}</span></h2>
      {#if c === "now" && list.length > nowLimit}
        <p class="wip" role="status">
          {list.length} projects in Now — more than {nowLimit} splits your focus. Consider pausing one.
        </p>
      {/if}
      <ol
        use:dragHandleZone={{ items: zones[c], flipDurationMs: flipMs, type: "card", dropTargetStyle: {}, dropTargetClasses: ["drop-target"] }}
        onconsider={(e) => consider(c, e)}
        onfinalize={(e) => finalize(c, e)}
        aria-label={`${columnText[c]} cards`}
      >
        {#each zones[c] as { id, p, row }, i (id)}
          {@const st = synth[p.project.path]}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions (the title button is the keyboard target) -->
          <li class="card" data-id={id} animate:flip={{ duration: flipMs }} onclick={(e) => cardClick(e, p)}>
            <span class="grip" use:dragHandle aria-label={`Drag ${name(p)}`}>⠿</span>
            <button class="link title" onclick={() => onOpen(p)} title={p.project.path}>{name(p)}</button>
            {#if st?.synthesis}
              <p class="desc" class:outdated={st.outdated} title={st.outdated ? "The project changed since this summary" : ""}>
                {st.synthesis.description}
              </p>
              {#if st.synthesis.next_steps[0]}<p class="step">→ {st.synthesis.next_steps[0]}</p>{/if}
            {/if}
            {#if st?.running}<p class="step muted">Summarizing…</p>{/if}
            {#if row.parking_note}<p class="parked" title={row.parking_note}>⏸ {row.parking_note}</p>{/if}
            {#if st?.error}<p class="step err" title={st.error}>Summary failed: {st.error}</p>{/if}
            <div class="badges">
              {#if p.staleness}
                <span class={`badge ${p.staleness.bucket}`}>{bucketText[p.staleness.bucket]} · {days(p)}</span>
              {/if}
              {#if p.mess}
                <span class={`badge ${p.mess.label}`} title={`Mess score ${Math.round(p.mess.score)}/100`}>
                  {messText[p.mess.label]}
                </span>
              {:else}
                <span class="badge neutral">No VCS</span>
              {/if}
              {#each p.activity?.agents ?? [] as a (a)}<span class="chip">{agentText[a]}</span>{/each}
            </div>
            <div class="controls">
              <button
                class="icon"
                aria-label={`Move ${name(p)} up`}
                disabled={i === 0}
                onclick={() => up(c, i)}>↑</button
              >
              <button
                class="icon"
                aria-label={`Move ${name(p)} down`}
                disabled={i === zones[c].length - 1}
                onclick={() => down(c, i)}>↓</button
              >
              <button
                class="icon"
                aria-label={`Refresh the summary of ${name(p)}`}
                title="Summarize again with the chosen agent"
                disabled={st?.running}
                onclick={() => onRefresh(p.project.path)}>↻</button
              >
              <select
                aria-label={`Column for ${name(p)}`}
                value={c}
                onchange={(e) => onMove(id, e.currentTarget.value as BoardColumn, null)}
              >
                {#each columns as target (target)}<option value={target}>{columnText[target]}</option>{/each}
              </select>
            </div>
          </li>
        {/each}
      </ol>
      {#if !list.length}<p class="empty muted">No projects</p>{/if}
    </section>
  {/each}
</div>

<style>
  /* UI-004: the board takes the rest of the window height and scrolls sideways itself (scrollbar
     at the window's bottom edge); each column scrolls its cards vertically. */
  .board {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: repeat(5, minmax(13rem, 1fr));
    grid-template-rows: minmax(0, 1fr);
    gap: 0.75rem;
    padding: 1rem 1.5rem;
    overflow-x: auto;
    overflow-y: hidden;
    align-items: start;
  }
  .column {
    display: flex;
    flex-direction: column;
    max-height: 100%;
    box-sizing: border-box;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 0.6rem;
    min-height: 8rem;
  }
  h2 {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-size: 0.85rem;
    margin: 0.15rem 0.2rem 0.65rem;
  }
  /* A color per column so it is recognized at a glance. */
  .col-dot {
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
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
  .count {
    color: var(--muted);
    font-weight: 500;
    font-size: 0.75rem;
    background: var(--gray-bg);
    border-radius: 999px;
    padding: 0 0.45rem;
  }
  .wip {
    background: var(--amber-bg);
    color: var(--amber);
    border: 1px solid var(--amber);
    border-radius: 6px;
    font-size: 0.78rem;
    padding: 0.4rem 0.5rem;
    margin: 0 0 0.6rem;
  }
  ol {
    flex: 1 1 auto;
    overflow-y: auto;
    /* Room for the drop-target outline and card shadows inside the scroll area. */
    padding: 3px;
    margin: -3px;
    min-height: 3rem;
    border-radius: 6px;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  :global(ol.drop-target) {
    outline: 2px dashed var(--accent);
    outline-offset: 2px;
  }
  .grip {
    float: right;
    cursor: grab;
    color: var(--muted);
    padding: 0 0.2rem;
    line-height: 1.2;
    user-select: none;
  }
  .card {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.6rem 0.65rem;
    box-shadow: var(--shadow);
    cursor: pointer;
    transition:
      box-shadow 0.12s,
      border-color 0.12s;
  }
  .card:hover {
    box-shadow: var(--shadow-hover);
    border-color: var(--muted);
  }
  /* Card controls stay out of the way until the card is hovered or focused (with a mouse); keyboard
     focus inside the card shows them too. Space is kept so cards don't jump. */
  @media (hover: hover) {
    .card .controls,
    .card .grip {
      opacity: 0;
      transition: opacity 0.12s;
    }
    .card:hover .controls,
    .card:hover .grip,
    .card:focus-within .controls,
    .card:focus-within .grip {
      opacity: 1;
    }
  }
  .title {
    font-weight: 600;
    color: var(--fg);
    padding: 0;
    text-align: left;
    overflow-wrap: anywhere;
  }
  .title:hover {
    color: var(--accent);
    text-decoration: underline;
  }
  .desc {
    margin: 0.35rem 0 0;
    font-size: 0.8rem;
    line-height: 1.3;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .desc.outdated {
    color: var(--muted);
    font-style: italic;
  }
  .step {
    margin: 0.3rem 0 0;
    font-size: 0.75rem;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err {
    color: var(--red);
  }
  .parked {
    margin: 0.3rem 0 0;
    font-size: 0.75rem;
    color: var(--amber);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badges {
    margin: 0.45rem 0 0.4rem;
  }
  .controls {
    display: flex;
    gap: 0.3rem;
    align-items: center;
  }
  .icon {
    padding: 0.1rem 0.45rem;
    font-size: 0.8rem;
  }
  select {
    font: inherit;
    font-size: 0.78rem;
    margin-left: auto;
    flex: 1;
    min-width: 0;
    max-width: 7rem;
    padding: 0.1rem 0.2rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
  }
  .empty {
    font-size: 0.8rem;
    margin: 0.3rem 0.2rem;
  }
</style>
