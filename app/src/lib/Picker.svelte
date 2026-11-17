<script lang="ts">
  // SYN-009: startup agent picker. Codex shows its exact plan usage (as of its last session);
  // Claude Code has no quota available locally, so it shows recent token usage, labeled an estimate.
  import { ago, when } from "./labels";
  import { backend } from "./backend";
  import type { Agent, ClaudeLimits, LimitWindow, PickerInfo } from "./types";

  let { info, onPick, onSkip }: { info: PickerInfo; onPick: (a: Agent) => void; onSkip: () => void } = $props();

  const m = $derived(info.meters);
  const limit = $derived(m.codex_limit);
  const compact = (n: number) =>
    n >= 1e9 ? `${(n / 1e9).toFixed(1)}B` : n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : String(n);
  const until = (iso: string | null) => (iso ? new Date(iso).toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" }) : "");
  const level = (p: number) => (p >= 95 ? "critical" : p >= 80 ? "warning" : "ok");
  const resetsSoon = (iso: string | null) =>
    iso ? new Date(iso).toLocaleString(undefined, { weekday: "short", hour: "2-digit", minute: "2-digit" }) : "";
  const live = (w: LimitWindow | null) => !!w && (!w.resets_at || Date.parse(w.resets_at) > Date.now());

  // Claude plan usage: the latest one Logpose saw, refreshable with a tiny call.
  let claudeLimits = $state<ClaudeLimits | null>(null);
  let checking = $state(false);
  let checkError = $state("");
  $effect(() => {
    claudeLimits = info.meters.claude_limit;
  });
  async function checkNow() {
    checking = true;
    checkError = "";
    try {
      claudeLimits = await backend.probeClaudeLimits();
    } catch (e) {
      checkError = String(e);
    } finally {
      checking = false;
    }
  }
  let first = $state<HTMLButtonElement>();
  $effect(() => {
    first?.focus();
  });
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && onSkip()} />

<div class="backdrop" role="presentation"></div>
<div class="picker" role="dialog" aria-modal="true" aria-labelledby="picker-title">
  <h2 id="picker-title">Summarize your projects?</h2>
  <p class="muted">
    The board is ready with git and session data. An agent can add a short summary and next step to each project; only
    projects that changed since their last summary are sent.
  </p>

  <div class="options">
    <section class="option">
      <h3>Claude Code</h3>
      {#if claudeLimits}
        {#each [{ label: "5-hour window", w: claudeLimits.five_hour, soon: true }, { label: "Weekly", w: claudeLimits.seven_day, soon: false }] as row (row.label)}
          {#if row.w}
            {@const pct = row.w.utilization * 100}
            <p class="window-label">{row.label}</p>
            {#if live(row.w)}
              <div
                class={`bar ${level(pct)}`}
                role="meter"
                aria-valuemin="0"
                aria-valuemax="100"
                aria-valuenow={Math.round(pct)}
                aria-label={`Claude Code ${row.label} usage`}
              >
                <span style:width={`${Math.min(100, pct)}%`}></span>
              </div>
              <p class="meter-text">
                {#if level(pct) !== "ok"}<strong>{level(pct) === "critical" ? "Nearly used up:" : "Running low:"}</strong>{/if}
                {Math.round(pct)}% used, {Math.round(100 - pct)}% left
                {#if row.w.resets_at}· resets {row.soon ? resetsSoon(row.w.resets_at) : until(row.w.resets_at)}{/if}
              </p>
            {:else}
              <p class="meter-text">Reset since the last check; usage unknown.</p>
            {/if}
          {/if}
        {/each}
        <p class="muted small" title={when(claudeLimits.observed_at)}>Exact as of {ago(claudeLimits.observed_at)}.</p>
      {:else}
        <p class="meter-text">Plan usage not checked yet.</p>
      {/if}
      <p class="muted small">
        <button class="link" onclick={checkNow} disabled={checking}>{checking ? "Checking…" : "Check now"}</button>
        (a tiny call, a few thousand mostly cached tokens)
      </p>
      {#if checkError}<p class="small err">{checkError}</p>{/if}
      <p class="muted small">
        Last 7 days: {m.claude_code.sessions} sessions · ≈{compact(m.claude_code.tokens)} tokens (+{compact(m.claude_code.cache_read)}
        cache reads) <span class="tag">estimate</span>
      </p>
      {#if m.claude_code.last_used}<p class="muted small" title={when(m.claude_code.last_used)}>Last used {ago(m.claude_code.last_used)}</p>{/if}
      <button class="primary" bind:this={first} onclick={() => onPick("claude-code")}>Summarize with Claude Code</button>
    </section>

    <section class="option">
      <h3>Codex</h3>
      {#if limit && !limit.expired}
        <div
          class={`bar ${level(limit.used_percent)}`}
          role="meter"
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={Math.round(limit.used_percent)}
          aria-label="Codex plan usage"
        >
          <span style:width={`${Math.min(100, limit.used_percent)}%`}></span>
        </div>
        <p class="meter-text">
          {#if level(limit.used_percent) !== "ok"}<strong>{level(limit.used_percent) === "critical" ? "Nearly used up:" : "Running low:"}</strong>{/if}
          {Math.round(limit.used_percent)}% of the {limit.window_minutes === 10080 ? "weekly" : "plan"} limit used,
          {Math.round(100 - limit.used_percent)}% left
          {#if limit.resets_at}· resets {until(limit.resets_at)}{/if}
        </p>
        <p class="muted small" title={when(limit.observed_at)}>Exact as of the last Codex session, {ago(limit.observed_at)}.</p>
      {:else if limit}
        <p class="meter-text">The limit window has reset since Codex was last used; current usage is unknown.</p>
      {:else}
        <p class="meter-text">No plan usage found in the Codex logs yet.</p>
      {/if}
      <p class="muted small">
        Last 7 days: {m.codex.sessions} sessions · ≈{compact(m.codex.tokens)} tokens (+{compact(m.codex.cache_read)} cache
        reads) <span class="tag">estimate</span>
      </p>
      <button class="primary" onclick={() => onPick("codex")}>Summarize with Codex</button>
    </section>
  </div>

  <div class="skip">
    <button onclick={onSkip}>Skip: show git and session data only</button>
    <span class="muted small">No tokens are spent. You can summarize later with Update summaries.</span>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.35);
    z-index: 20;
  }
  .picker {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(44rem, calc(100% - 2rem));
    max-height: calc(100% - 2rem);
    overflow-y: auto;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 1.25rem 1.5rem;
    z-index: 21;
    box-sizing: border-box;
    box-shadow: 0 10px 30px rgb(0 0 0 / 0.25);
  }
  h2 {
    margin: 0 0 0.4rem;
    font-size: 1.15rem;
  }
  h3 {
    margin: 0 0 0.5rem;
    font-size: 0.95rem;
  }
  p {
    margin: 0 0 0.5rem;
    font-size: 0.85rem;
    line-height: 1.45;
  }
  .options {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
    gap: 0.9rem;
    margin: 1rem 0;
  }
  .option {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.8rem 0.9rem;
    display: flex;
    flex-direction: column;
  }
  .option button {
    margin-top: auto;
  }
  .option .small:last-of-type {
    margin-bottom: 1rem;
  }
  .small {
    font-size: 0.78rem;
  }
  .tag {
    font-size: 0.7rem;
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0 0.4rem;
    color: var(--muted);
    margin-left: 0.2rem;
  }
  .bar {
    height: 8px;
    border-radius: 999px;
    background: var(--gray-bg);
    overflow: hidden;
    margin-bottom: 0.5rem;
  }
  .bar span {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--accent);
  }
  .bar.warning span {
    background: var(--amber);
  }
  .bar.critical span {
    background: var(--red);
  }
  .window-label {
    font-size: 0.75rem;
    color: var(--muted);
    margin: 0.5rem 0 0.3rem;
  }
  .err {
    color: var(--red);
  }
  .skip {
    display: flex;
    gap: 0.8rem;
    align-items: center;
    flex-wrap: wrap;
  }
</style>
