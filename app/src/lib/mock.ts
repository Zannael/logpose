// Synthetic data for previewing the UI in a browser. Not used inside the Tauri app.
// Add `?setup` to the URL to start on the first-launch screen. The board is kept in localStorage
// (like the SQLite store in the app) so a reload shows whether column/order persist; `?reset` clears it.

import type { Backend } from "./backend";
import type { RemovalItem, RemovalPlan } from "./types";
import type { Agent, AgentCheck, AgentSplit, Known, SynthesisSummary, SynthesisUpdate, Synthesis, BoardColumn, BoardRow, Bucket, Commit, ClaudeLimits, Locations, Neglected, PickerInfo, ProjectReport, RepoReport, RepoSignals, RuleStatus, ScanReport, Settings, StatsView } from "./types";

const day = 86_400_000;
const ago = (d: number) => new Date(Date.now() - d * day).toISOString();

function repo(path: string, days: number, over: Partial<RepoSignals> = {}): RepoReport {
  return {
    path,
    signals: {
      last_commit_at: ago(days),
      branch: "main",
      default_branch: "main",
      on_default_branch: true,
      staged: 0,
      modified: 2,
      untracked: 1,
      conflicted: 0,
      upstream: "refs/remotes/origin/main",
      ahead: 0,
      behind: 0,
      head_id: "4f2a9c1e0b",
      stash_count: 0,
      oldest_stash_at: null,
      operation: null,
      ...over,
    },
    dirty: { lines_added: 42, lines_removed: 7, oldest_change_at: ago(days + 1) },
    unmerged_branches: [],
    todos_added: 0,
    error: null,
  };
}

function project(
  title: string,
  days: number,
  bucket: Bucket,
  mess: ProjectReport["mess"],
  agents: Agent[],
  extra: Partial<ProjectReport> = {},
): ProjectReport {
  const path = `/home/user/dev/${title}`;
  return {
    project: { path, title, root: "/home/user/dev", alias: null, from_logs: false, sub_repos: [] },
    repo: mess ? repo(path, days) : null,
    sub_repos: [],
    mtime: { has_vcs: mess !== null, latest: { modified_at: ago(days), path: `${path}/src/main.rs` }, files_seen: 84 },
    activity: agents.length
      ? {
          last_session_at: ago(days),
          last_agent: agents[0],
          agents,
          sessions_7d: 1,
          sessions_30d: 3,
          sessions_total: 12,
          last_prompts: [
            { at: ago(days + 0.2), text: "Add a price alert command" },
            { at: ago(days + 0.1), text: "Write tests for the alert threshold parsing" },
            { at: ago(days), text: "Finish the price alert command" },
          ],
          final_message: "Added `/alert` with threshold parsing and tests; the Telegram send is still stubbed.",
          title: "Price alerts",
          tokens: { input: 18_200, output: 4_100, cache_read: 220_000, cache_creation: 9_000 },
        }
      : null,
    mess_inputs: mess
      ? {
          operation_in_progress: false,
          uncommitted_files: 3,
          oldest_uncommitted_days: days + 1,
          unpushed_commits: 0,
          stash_count: 0,
          stale_branches: 0,
          no_upstream: false,
          days_inactive: days,
        }
      : null,
    staleness: { days, bucket, last_activity: ago(days) },
    mess,
    ...extra,
  };
}

const switchPath = "/home/user/switch/sysmodule-foo";
const sample: ProjectReport[] = [
  project("deals-bot", 0.4, "fresh", { score: 12, label: "clean" }, ["claude-code", "codex"]),
  project("sysmodule-foo", 2.2, "fresh", { score: 35, label: "tidy-up" }, ["codex"], {
    project: {
      path: switchPath,
      title: "sysmodule-foo",
      root: "/home/user/switch",
      alias: "Switch sysmodule",
      from_logs: false,
      sub_repos: [`${switchPath}/Atmosphere`, `${switchPath}/libnx`],
    },
    repo: repo(switchPath, 2.2, { branch: "feature/overlay", on_default_branch: false, ahead: 2, stash_count: 1, oldest_stash_at: ago(9) }),
    sub_repos: [
      { ...repo(`${switchPath}/Atmosphere`, 30, { upstream: null, modified: 5 }), todos_added: 2 },
      {
        ...repo(`${switchPath}/libnx`, 60, { modified: 0, untracked: 0 }),
        dirty: { lines_added: 0, lines_removed: 0, oldest_change_at: null },
        unmerged_branches: [{ name: "fix-ipc", tip_at: ago(70) }],
      },
    ],
  }),
  project("roaming-agent", 41, "stale", { score: 64, label: "abandoned-mid-way" }, ["codex"], {
    repo: repo("/home/user/dev/roaming-agent", 41, { operation: "rebase", conflicted: 1 }),
  }),
  project("research-notes", 9, "cooling", null, ["claude-code"], {
    project: { path: "/home/user/research/notes", title: "research-notes", root: "/home/user/research", alias: null, from_logs: true, sub_repos: [] },
  }),
  project("flashcards", 20, "stale", { score: 22, label: "abandoned-mid-way" }, [], {
    repo: null,
    mtime: { has_vcs: false, latest: { modified_at: ago(20), path: "/home/user/dev/flashcards/ios/README.md" }, files_seen: 40 },
    project: {
      path: "/home/user/dev/flashcards",
      title: "flashcards",
      root: "/home/user/dev",
      alias: null,
      from_logs: false,
      sub_repos: ["/home/user/dev/flashcards/ios", "/home/user/dev/flashcards/android"],
    },
    sub_repos: [repo("/home/user/dev/flashcards/ios", 20), repo("/home/user/dev/flashcards/android", 25, { modified: 0, untracked: 0 })],
  }),
  project("old-website", 242, "dormant", { score: 2, label: "clean" }, []),
];

const boardKey = "logpose-mock-board";
const params = new URLSearchParams(globalThis.location?.search ?? "");

function loadBoard(): BoardRow[] {
  try {
    if (params.has("reset")) localStorage.removeItem(boardKey);
    return JSON.parse(localStorage.getItem(boardKey) ?? "[]");
  } catch {
    return [];
  }
}

let board: BoardRow[] = loadBoard();

function saveBoard() {
  try {
    localStorage.setItem(boardKey, JSON.stringify(board));
  } catch {
    // Preview only: without storage the board just doesn't survive a reload.
  }
}

/** Same rules as `Store::upsert_project` + `scan::persist`: new paths go to the end of Next. */
function persist(projects: ProjectReport[]) {
  for (const p of projects) {
    const row = board.find((r) => r.path === p.project.path);
    if (row) row.title = p.project.title;
    else
      board.push({
        id: Math.max(0, ...board.map((r) => r.id)) + 1,
        path: p.project.path,
        title: p.project.title,
        alias: null,
        column: "next",
        position: board.filter((r) => r.column === "next").length,
        archived_at: null,
        maintenance_at: null,
        parking_note: null,
        private: false,
      });
  }
  saveBoard();
}

function ordered(): BoardRow[] {
  return structuredClone(board).sort(
    (a, b) => columnIndex(a.column) - columnIndex(b.column) || a.position - b.position || a.id - b.id,
  );
}

const columnIndex = (c: BoardColumn) => ["now", "next", "paused", "maintenance", "archived"].indexOf(c);

let roots: string[] = params.has("setup") ? [] : ["~/dev", "~/switch"];

let exclude: string[] = ["/home/user/dev/work-*"];
let aliases: Record<string, string> = { [switchPath]: "Switch sysmodule" };

const rules: Record<string, string[]> = {};
// Mock synthesis cache: path → synthesis. One project starts with an outdated entry.
const synthCache: Record<string, Synthesis> = {};

function fakeSynthesis(p: ProjectReport, agent: string): Synthesis {
  return {
    description: `${p.project.alias ?? p.project.title}: a small ${p.mess ? "git" : "notes"} project (sample summary by ${agent}).`,
    state: p.mess?.label === "abandoned-mid-way" ? "Half-finished change left uncommitted for weeks." : "Works; a few loose ends.",
    last_goal: p.activity?.last_prompts.at(-1)?.text ?? "",
    next_steps: ["Commit or drop the pending changes", "Write the missing tests", "Update the README"],
    blockers: p.repo?.signals?.operation ? [`A ${p.repo.signals.operation} is in progress`] : [],
    confidence: "medium",
  };
}
let digestOn = true;
let trayOn = false;
let mockClaudeLimits: ClaudeLimits = {
  observed_at: ago(0.05),
  five_hour: { utilization: 0.47, resets_at: ago(-0.07) },
  seven_day: { utilization: 0.05, resets_at: ago(-6.9) },
};

export const mockBackend: Backend = {
  async knownSyntheses() {
    const out: { path: string; known: Known }[] = [];
    for (const p of sample) {
      const s = synthCache[p.project.path];
      if (s) out.push({ path: p.project.path, known: { synthesis: s, source: "cache", outdated: false, agent: "codex", created_at: null } });
    }
    const old = sample.find((p) => p.project.title === "roaming-agent");
    if (old && !synthCache[old.project.path])
      out.push({ path: old.project.path, known: { synthesis: fakeSynthesis(old, "claude-code"), source: "cache", outdated: true, agent: "claude-code", created_at: ago(30) } });
    return out;
  },
  async synthesize(agent, paths, force, onUpdate): Promise<SynthesisSummary> {
    const summary: SynthesisSummary = { next_md: 0, cached: 0, ran: 0, failed: 0, batches: 0, fallbacks: 0 };
    const targets = sample.filter((p) => !paths || paths.includes(p.project.path));
    const update = (u: Omit<SynthesisUpdate, "run">) => onUpdate({ run: "mock", ...u });
    for (const p of targets) {
      const path = p.project.path;
      if (!force && synthCache[path]) {
        summary.cached++;
        update({ path, ok: true, synthesis: synthCache[path], source: "cache", error: null });
      }
    }
    const todo = targets.filter((p) => force || !synthCache[p.project.path]);
    for (const p of todo) {
      await new Promise((r) => setTimeout(r, 400));
      const path = p.project.path;
      if (p.project.title === "old-website") {
        summary.failed++;
        update({ path, ok: false, synthesis: null, source: null, error: "agent timed out after 180s" });
        continue;
      }
      summary.ran++;
      synthCache[path] = fakeSynthesis(p, agent);
      update({ path, ok: true, synthesis: synthCache[path], source: "agent", error: null });
    }
    return summary;
  },
  async setParkingNote(id, note) {
    const row = board.find((r) => r.id === id);
    if (row) row.parking_note = note?.split(/\s+/).filter(Boolean).join(" ").slice(0, 200) || null;
    saveBoard();
    return ordered();
  },
  async setPrivate(id, priv) {
    const row = board.find((r) => r.id === id);
    if (row) row.private = priv;
    saveBoard();
    return ordered();
  },
  async trayEnabled() {
    return trayOn;
  },
  async setTrayEnabled(on) {
    trayOn = on;
    return trayOn;
  },
  async onBackgroundRescan() {
    return () => {};
  },
  async launchAgent() {
    return "/usr/bin/gnome-terminal";
  },
  async openProject(path) {
    const kind = path.includes("notes") ? "other" : "code";
    return { program: kind === "other" ? "/usr/bin/xdg-open" : "/usr/bin/webstorm", kind, fell_back: false };
  },
  async restorePlacement() {
    // Same rules as logpose-core::board::suggest, on the sample data.
    const ordered = [...sample].sort((a, b) => (a.staleness?.days ?? Infinity) - (b.staleness?.days ?? Infinity));
    const now = ordered.filter((p) => p.staleness?.bucket === "fresh").slice(0, 3);
    const column = (p: ProjectReport): BoardColumn => {
      if (now.includes(p)) return "now";
      const b = p.staleness?.bucket;
      if (!b || b === "fresh" || b === "cooling") return "next";
      if (b === "stale") return "paused";
      return p.mess?.label === "clean" ? "maintenance" : "archived";
    };
    const counters: Record<string, number> = {};
    board = ordered.flatMap((p) => {
      const row = board.find((r) => r.path === p.project.path);
      if (!row) return [];
      const c = column(p);
      const position = (counters[c] = (counters[c] ?? -1) + 1);
      return [{ ...row, column: c, position, archived_at: c === "archived" ? (row.archived_at ?? new Date().toISOString()) : null, maintenance_at: c === "maintenance" ? (row.maintenance_at ?? new Date().toISOString()) : null }];
    });
    saveBoard();
    return ordered.length ? (this as Backend).board() : [];
  },
  async probeClaudeLimits(): Promise<ClaudeLimits> {
    await new Promise((r) => setTimeout(r, 800));
    mockClaudeLimits = { ...mockClaudeLimits, observed_at: new Date().toISOString(), five_hour: { utilization: 0.51, resets_at: ago(-0.06) } };
    return mockClaudeLimits;
  },
  async agentPicker(): Promise<PickerInfo> {
    await new Promise((r) => setTimeout(r, 200));
    return {
      default_agent: params.has("autopick") ? "claude" : "ask",
      meters: {
        claude_code: { sessions: 12, tokens: 1_840_000, cache_read: 420_000_000, last_used: ago(0.3) },
        codex: { sessions: 31, tokens: 5_200_000, cache_read: 90_000_000, last_used: ago(0.1) },
        claude_limit: params.has("noclaude") ? null : mockClaudeLimits,
        codex_limit: { used_percent: 68, window_minutes: 10_080, resets_at: ago(-2.5), observed_at: ago(0.1), plan_type: "plus", expired: false },
      },
    };
  },
  async digestIfDue(): Promise<Neglected[]> {
    return [];
  },
  async digestEnabled() {
    return digestOn;
  },
  async setDigestEnabled(on) {
    digestOn = on;
    return digestOn;
  },
  async agentRuleStatus(path): Promise<RuleStatus> {
    return { files: rules[path] ?? [] };
  },
  async setAgentRule(path, enabled, agents) {
    rules[path] = enabled ? [...new Set(agents.map((a) => (a === "claude-code" ? "CLAUDE.md" : "AGENTS.md")).concat(agents.length ? [] : ["AGENTS.md"]))] : [];
    return this.agentRuleStatus(path);
  },
  async getSettings(): Promise<Settings> {
    return { config_path: "~/.config/logpose/config.toml", locations: { roots: [...roots], exclude: [...exclude], aliases: { ...aliases } } };
  },
  async saveSettings(loc: Locations) {
    const next = loc.roots.map((r) => r.trim()).filter(Boolean);
    if (!next.length) throw "add at least one folder";
    if (loc.exclude.some((g) => g.includes("["))) throw "invalid exclude glob: unclosed character class";
    roots = next;
    exclude = loc.exclude.map((g) => g.trim()).filter(Boolean);
    aliases = Object.fromEntries(Object.entries(loc.aliases).filter(([, a]) => a.trim()));
    return this.getSettings();
  },
  async getSetup() {
    return { config_path: "~/.config/logpose/config.toml", roots: [...roots], needs_setup: roots.length === 0 };
  },
  async saveRoots(next) {
    if (next.length === 0) throw "add at least one folder";
    roots = [...next];
    return this.getSetup();
  },
  async scan(): Promise<ScanReport> {
    await new Promise((r) => setTimeout(r, 300));
    const first = board.length === 0;
    persist(sample.filter((p) => !removed.has(p.project.path)));
    if (first) await mockBackend.restorePlacement(); // first launch: suggested columns (BOARD-003)
    const projects = sample.filter((p) => !removed.has(p.project.path)).map((p) => ({ ...p, project: { ...p.project, alias: aliases[p.project.path] ?? null } }));
    return { taken_at: new Date().toISOString(), projects, duration: { secs: 0, nanos: 300_000_000 } };
  },
  async board() {
    return ordered();
  },
  async moveCard(id, column, before) {
    const moving = board.find((r) => r.id === id);
    if (!moving) throw `no project ${id}`;
    const cols = new Map<BoardColumn, BoardRow[]>();
    for (const r of ordered()) if (r.id !== id) cols.set(r.column, [...(cols.get(r.column) ?? []), r]);
    const target = cols.get(column) ?? [];
    const at = target.findIndex((r) => r.id === before);
    target.splice(at < 0 ? target.length : at, 0, { ...moving, column });
    cols.set(column, target);
    board = [...cols.values()].flatMap((rows) =>
      rows.map((r, i) => ({
        ...r,
        position: i,
        archived_at: r.column === "archived" ? (r.archived_at ?? new Date().toISOString()) : null,
        maintenance_at: r.column === "maintenance" ? (r.maintenance_at ?? new Date().toISOString()) : null,
      })),
    );
    saveBoard();
    return ordered();
  },
  async recentCommits(path, limit): Promise<Commit[]> {
    const subjects = ["Fix flaky test", "Add config loader", "Refactor scanner", "Bump deps", "Initial commit"];
    const p = sample.find((x) => x.repo?.path === path || x.sub_repos.some((r) => r.path === path));
    const age = [p?.repo, ...(p?.sub_repos ?? [])].find((r) => r?.path === path)?.signals?.last_commit_at;
    if (!age) throw `not a git repository: ${path}`;
    const t0 = Date.parse(age);
    return subjects.slice(0, limit).map((subject, i) => ({ at: new Date(t0 - i * 1.5 * day).toISOString(), subject }));
  },
  async stats(projects, from, to): Promise<StatsView> {
    const end = to ? Date.parse(`${to}T12:00:00`) : Date.now();
    const days = Math.round((end - Date.parse(`${from}T12:00:00`)) / day);
    await new Promise((r) => setTimeout(r, 200));
    // Deterministic pseudo-random series so the preview is stable across reloads.
    let seed = 7;
    const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
    const key = (d: Date) => d.toISOString().slice(0, 10);
    const monday = (d: Date) => new Date(d.getTime() - ((d.getUTCDay() + 6) % 7) * day);
    const commits: Record<string, number> = {};
    const weeks: Record<string, number> = {};
    const perWeek: Record<string, AgentSplit> = {};
    const use = (s: number, p: number) => ({ sessions: s, prompts: p, tokens: p * 9_000 });
    for (let i = days; i >= 0; i--) {
      const d = new Date(end - i * day);
      const n = rnd() < 0.45 ? 0 : Math.floor(rnd() * rnd() * 14);
      if (n) commits[key(d)] = n;
      const w = key(monday(d));
      if (n) weeks[w] = Math.min(projects.length, (weeks[w] ?? 0) + (rnd() < 0.4 ? 1 : 0) || 1);
      const c = rnd() < 0.3 ? 1 : 0;
      const x = rnd() < 0.6 ? 1 + Math.floor(rnd() * 2) : 0;
      if (i < 120 && (c || x)) {
        const prev = perWeek[w] ?? { claude_code: use(0, 0), codex: use(0, 0) };
        perWeek[w] = { claude_code: use(prev.claude_code.sessions + c, prev.claude_code.prompts + c * 4), codex: use(prev.codex.sessions + x, prev.codex.prompts + x * 3) };
      }
    }
    const perProject: Record<string, AgentSplit> = {};
    sample.forEach((p, i) => {
      if (p.activity) perProject[p.project.path] = { claude_code: use(8 - i, 30 - i * 3), codex: use(3 + i * 4, 12 + i * 9) };
    });
    const scanDays = [9, 6, 5, 2, 1, 0].map((i) => key(new Date(Date.now() - i * day)));
    const buckets = Object.fromEntries(
      scanDays.map((d, i) => [d, { fresh: 2 + (i % 2), cooling: 1, stale: 2 - (i % 2), dormant: 1, unknown: 0 }]),
    );
    const month = (n: number) => { const d = new Date(); d.setUTCDate(1); d.setUTCMonth(d.getUTCMonth() - n); return key(d); };
    return {
      git: { commits_per_day: commits, projects_per_week: weeks, emails: ["me@example.com"], errors: [] },
      sessions: { per_project: perProject, per_week: perWeek, unattributed: { claude_code: use(2, 5), codex: use(4, 9) } },
      buckets,
      moves: { [month(1)]: { archived: 1, maintenance: 0 }, [month(0)]: { archived: 0, maintenance: 1 } },
    };
  },
  async checkAgent(agent): Promise<AgentCheck> {
    await new Promise((r) => setTimeout(r, 600));
    return agent === "claude-code"
      ? { agent, program: "/usr/bin/claude", ok: true, model: "claude-haiku-5-5", tokens: 2_336, millis: 7_900, error: null }
      : { agent, program: "/usr/lib/chatgpt/resources/codex", ok: false, model: null, tokens: 0, millis: 1_200,
          error: "agent exited with exit status: 2: error: unexpected argument '--ephemeral' found" };
  },
  async pickFolder() {
    return `/home/user/folder-${roots.length + 1}`;
  },
  async removalPlan(path) {
    await new Promise((r) => setTimeout(r, 900));
    const name = path.split("/").pop()!;
    const GB = 1e9;
    const item = (id: string, group: RemovalItem["group"], label: string, bytes: number, extra: Partial<RemovalItem> = {}): RemovalItem => ({
      id, group, label, bytes, detail: null, certainty: "certain", selected: true, required: false, checks: [], action: { kind: "x" }, ...extra,
    });
    const items: RemovalItem[] = [
      item("folder", "folder", path, 3.78 * GB, { required: true, detail: "Moved to the Trash; restore it from the file manager if needed." }),
      item("container:c1", "containers", `${name}-web-1`, 108_000, { detail: `exited · image ${name}-web:latest` }),
      item("container:c2", "containers", `${name}-db-1`, 64_000, { detail: "exited · image postgres:16" }),
      item("network:n1", "containers", `network ${name}_default`, 0, { detail: "compose network of these containers" }),
      item("volume:v1", "volumes", `${name}_pgdata`, 0.84 * GB, { detail: `mounted by ${name}-db-1` }),
      item("volume:v2", "volumes", `${name}-ops_cache`, 0.12 * GB, {
        certainty: "probable", selected: false,
        detail: `compose project “${name}-ops”: the name a compose file in this folder would use, but no container confirms it`,
        checks: [`docker volume inspect ${name}-ops_cache`, `docker ps -a --filter volume=${name}-ops_cache`, `docker run --rm -v ${name}-ops_cache:/data alpine ls -la /data`],
      }),
      item("image:i1", "images", `${name}-web:latest`, 1.92 * GB, { detail: "built for this project; used only by web-1" }),
      item("image:i2", "images", "postgres:16", 0.43 * GB, { detail: "pulled from a registry (can be downloaded again); used only by db-1" }),
      ...Array.from({ length: 6 }, (_, k) =>
        item(`image:old${k}`, "images", `<untagged> 5b46${k}0615f9f`, k === 0 ? 1.96 * GB : 0, { detail: `built for compose project “${name}”; no container uses it` }),
      ),
      item("image:i3", "images", `${name}-ops:latest`, 0.25 * GB, {
        certainty: "probable", selected: false,
        detail: `built for compose project “${name}-ops”: the name a compose file in this folder would use, but no container confirms it`,
        checks: ["docker image inspect 0a7522c0e9da --format '{{json .Config.Labels}}'", "docker image history 0a7522c0e9da"],
      }),
      item("logs:claude", "agent-logs", "Claude Code: 5 sessions (10 files)", 0.03 * GB, { selected: false, detail: "Kept unless you tick it. Moving them to the Trash also removes them from Logpose's history and Stats." }),
      item("logs:codex", "agent-logs", "Codex: 3 sessions (3 files)", 0.002 * GB, { selected: false, detail: "Kept unless you tick it. Moving them to the Trash also removes them from Logpose's history and Stats." }),
      item("logpose", "logpose", "Board card, parking note, cached summaries and scan history", 0, { required: true, detail: "Always cleared." }),
    ];
    const plan: RemovalPlan = {
      project: path,
      items,
      build_outputs: [
        { path: `${path}/target`, bytes: 2.9 * GB },
        { path: `${path}/app/node_modules`, bytes: 0.16 * GB },
      ],
      repos: [
        {
          path,
          remotes: [{ name: "origin", url: `git@github.com:user/${name}.git`, web_url: `https://github.com/user/${name}` }],
          branches: [
            { name: "main", upstream: "origin/main", ahead: 2 },
            { name: "spike/cache", upstream: null, ahead: 5 },
          ],
          uncommitted: 3,
          stashes: 1,
          error: null,
        },
      ],
      docker: { state: "available" },
      millis: 900,
    };
    return plan;
  },
  async checkRemote(url) {
    await new Promise((r) => setTimeout(r, 1200));
    if (url.includes("research")) throw "fatal: repository not found";
    return 4;
  },
  async removeProject(path, selected, onStep) {
    const plan = await mockBackend.removalPlan(path);
    const todo = plan.items.filter((i) => i.required || selected.includes(i.id));
    const steps = [];
    for (const i of todo) {
      await new Promise((r) => setTimeout(r, 250));
      const step = { id: i.id, ok: true, error: null, bytes: i.bytes, trashed: i.group === "folder" || i.group === "agent-logs" };
      steps.push(step);
      onStep(step);
    }
    removed.add(path);
    const docker = todo.filter((i) => ["containers", "volumes", "images"].includes(i.group)).reduce((a, i) => a + i.bytes, 0);
    return { steps, docker_freed: Math.round(docker * 1.18) };
  },
  async openUrl(url) {
    window.open(url, "_blank");
  },
};

/** Projects removed in this preview session (DEL-002). */
const removed = new Set<string>();
