<script lang="ts">
  // Vertical (stacked) bar chart in SVG: thin bars, 2px gaps between segments, rounded top end,
  // recessive grid, hover tooltip per bar, and a table view for exact values.
  type Series = { label: string; color: string };
  type Row = { label: string; tip?: string; values: number[] };

  let { series, rows, caption, height = 160 }: { series: Series[]; rows: Row[]; caption: string; height?: number } =
    $props();

  const W = 720;
  const pad = { top: 8, right: 8, bottom: 22, left: 32 };
  const plotH = $derived(height - pad.top - pad.bottom);
  const max = $derived(Math.max(1, ...rows.map((r) => r.values.reduce((a, b) => a + b, 0))));
  const step = $derived(niceStep(max));
  const top = $derived(Math.ceil(max / step) * step);
  const slot = $derived((W - pad.left - pad.right) / Math.max(1, rows.length));
  const barW = $derived(Math.max(2, Math.min(22, slot * 0.7)));
  const labelEvery = $derived(Math.max(1, Math.ceil(rows.length / 8)));
  let hover = $state<number | null>(null);

  function niceStep(m: number) {
    const raw = m / 3;
    const p = 10 ** Math.floor(Math.log10(raw));
    return [1, 2, 5, 10].map((k) => k * p).find((s) => s >= raw) ?? raw;
  }

  const y = (v: number) => pad.top + plotH - (v / top) * plotH;

  /** Rect with only the top corners rounded. */
  function topRounded(x: number, y0: number, w: number, h: number, r: number) {
    r = Math.min(r, w / 2, h);
    return `M${x},${y0 + h}V${y0 + r}Q${x},${y0} ${x + r},${y0}H${x + w - r}Q${x + w},${y0} ${x + w},${y0 + r}V${y0 + h}Z`;
  }

  function segments(r: Row) {
    let acc = 0;
    const out: { y: number; h: number; color: string; last: boolean }[] = [];
    const lastIdx = r.values.findLastIndex((v) => v > 0);
    r.values.forEach((v, i) => {
      if (v <= 0) return;
      const y0 = y(acc + v);
      const y1 = y(acc);
      acc += v;
      // 2px surface gap below every segment except the bottom one.
      const gap = out.length ? 2 : 0;
      out.push({ y: y0, h: Math.max(1, y1 - y0 - gap), color: series[i].color, last: i === lastIdx });
    });
    return out;
  }
</script>

<figure>
  {#if series.length > 1}
    <ul class="legend">
      {#each series as s (s.label)}<li><span class="key" style:background={s.color}></span>{s.label}</li>{/each}
    </ul>
  {/if}
  <div class="wrap">
    <svg viewBox={`0 0 ${W} ${height}`} role="img" aria-label={caption}>
      {#each Array.from({ length: Math.round(top / step) + 1 }, (_, i) => i * step) as t (t)}
        <line class="grid" x1={pad.left} x2={W - pad.right} y1={y(t)} y2={y(t)} />
        <text class="tick" x={pad.left - 6} y={y(t) + 3.5} text-anchor="end">{t}</text>
      {/each}
      {#each rows as r, i (r.label)}
        {@const x = pad.left + i * slot + (slot - barW) / 2}
        {#each segments(r) as s, k (k)}
          {#if s.last}
            <path d={topRounded(x, s.y, barW, s.h, 4)} fill={s.color} />
          {:else}
            <rect {x} y={s.y} width={barW} height={s.h} fill={s.color} />
          {/if}
        {/each}
        {#if i % labelEvery === 0}
          <text class="tick" x={x + barW / 2} y={height - 6} text-anchor="middle">{r.label}</text>
        {/if}
        <rect
          class="hit"
          x={pad.left + i * slot}
          y={pad.top}
          width={slot}
          height={plotH}
          role="presentation"
          onmouseenter={() => (hover = i)}
          onmouseleave={() => (hover = null)}
        />
      {/each}
      {#if hover !== null}
        {@const hx = pad.left + hover * slot + slot / 2}
        <line class="cross" x1={hx} x2={hx} y1={pad.top} y2={pad.top + plotH} />
      {/if}
    </svg>
    {#if hover !== null}
      {@const r = rows[hover]}
      <div class="tip" style:left={`${((pad.left + hover * slot + slot / 2) / W) * 100}%`}>
        <strong>{r.tip ?? r.label}</strong>
        {#each series as s, k (s.label)}
          <div><span class="key" style:background={s.color}></span>{s.label}: {r.values[k]}</div>
        {/each}
      </div>
    {/if}
  </div>
  <details>
    <summary>Table</summary>
    <table>
      <thead><tr><th scope="col"></th>{#each series as s (s.label)}<th scope="col">{s.label}</th>{/each}</tr></thead>
      <tbody>
        {#each rows as r (r.label)}
          <tr><th scope="row">{r.tip ?? r.label}</th>{#each r.values as v, k (k)}<td>{v}</td>{/each}</tr>
        {/each}
      </tbody>
    </table>
  </details>
</figure>

<style>
  figure {
    margin: 0;
  }
  .wrap {
    position: relative;
  }
  svg {
    width: 100%;
    height: auto;
    display: block;
  }
  .grid {
    stroke: var(--border);
    stroke-width: 1;
  }
  .tick {
    fill: var(--muted);
    font-size: 10px;
    font-variant-numeric: tabular-nums;
  }
  .hit {
    fill: transparent;
  }
  .cross {
    stroke: var(--muted);
    stroke-width: 1;
    stroke-dasharray: 2 3;
    pointer-events: none;
  }
  .tip {
    position: absolute;
    top: 0;
    transform: translateX(-50%);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.35rem 0.5rem;
    font-size: 0.75rem;
    pointer-events: none;
    white-space: nowrap;
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.12);
    z-index: 1;
  }
  .legend {
    display: flex;
    gap: 1rem;
    list-style: none;
    margin: 0 0 0.4rem;
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
  details {
    font-size: 0.78rem;
    color: var(--muted);
    margin-top: 0.3rem;
  }
  table {
    border-collapse: collapse;
    margin-top: 0.3rem;
  }
  th,
  td {
    padding: 0.15rem 0.75rem 0.15rem 0;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  th[scope="row"] {
    text-align: left;
    font-weight: normal;
  }
</style>
