// Subset of the Rust types serialized by the Tauri commands (logpose-core::scan / app setup).

export type Agent = "claude-code" | "codex";
export type Bucket = "fresh" | "cooling" | "stale" | "dormant";
export type MessLabel = "clean" | "tidy-up" | "abandoned-mid-way";

export interface SetupState {
  config_path: string;
  roots: string[];
  needs_setup: boolean;
}

export type Operation = "merge" | "rebase" | "cherry-pick" | "revert" | "bisect" | "apply-mailbox";

export interface RepoSignals {
  last_commit_at: string | null;
  branch: string | null;
  default_branch: string | null;
  on_default_branch: boolean;
  staged: number;
  modified: number;
  untracked: number;
  conflicted: number;
  upstream: string | null;
  ahead: number;
  behind: number;
  head_id: string | null;
  stash_count: number;
  oldest_stash_at: string | null;
  operation: Operation | null;
}

export interface RepoReport {
  path: string;
  signals: RepoSignals | null;
  dirty: { lines_added: number; lines_removed: number; oldest_change_at: string | null } | null;
  unmerged_branches: { name: string; tip_at: string | null }[];
  todos_added: number;
  error: string | null;
}

export interface ProjectReport {
  project: {
    path: string;
    title: string;
    root: string;
    alias: string | null;
    from_logs: boolean;
    sub_repos: string[];
  };
  repo: RepoReport | null;
  sub_repos: RepoReport[];
  mtime: { has_vcs: boolean; latest: { modified_at: string; path: string } | null; files_seen: number };
  activity: {
    last_session_at: string;
    last_agent: Agent;
    agents: Agent[];
    sessions_7d: number;
    sessions_30d: number;
    sessions_total: number;
    last_prompts: { at: string | null; text: string }[];
    final_message: string | null;
    title: string | null;
    tokens: { input: number; output: number; cache_read: number; cache_creation: number };
  } | null;
  mess_inputs: {
    operation_in_progress: boolean;
    uncommitted_files: number;
    oldest_uncommitted_days: number | null;
    unpushed_commits: number;
    stash_count: number;
    stale_branches: number;
    no_upstream: boolean;
    days_inactive: number | null;
  } | null;
  staleness: { days: number; bucket: Bucket; last_activity: string } | null;
  mess: { score: number; label: MessLabel } | null;
}

export interface Commit {
  at: string;
  subject: string;
}

export interface ScanReport {
  taken_at: string;
  projects: ProjectReport[];
  duration: { secs: number; nanos: number };
}

export type BoardColumn = "now" | "next" | "paused" | "maintenance" | "archived";

/** A row of the SQLite `projects` table (logpose-core::store::ProjectRow). */
export interface BoardRow {
  id: number;
  path: string;
  title: string;
  alias: string | null;
  column: BoardColumn;
  position: number;
  archived_at: string | null;
  maintenance_at: string | null;
  parking_note: string | null;
  /** Privacy opt-out (SYN-008): prompts and agent messages stay out of the context pack. */
  private: boolean;
}

export interface AgentUsage {
  sessions: number;
  prompts: number;
  tokens: number;
}
export interface AgentSplit {
  claude_code: AgentUsage;
  codex: AgentUsage;
}
export interface BucketCounts {
  fresh: number;
  cooling: number;
  stale: number;
  dormant: number;
  unknown: number;
}

/** `stats` command (STATS-001..003). Date keys are local `YYYY-MM-DD`. */
export interface StatsView {
  git: {
    commits_per_day: Record<string, number>;
    projects_per_week: Record<string, number>;
    emails: string[];
    errors: string[];
  };
  sessions: {
    per_project: Record<string, AgentSplit>;
    per_week: Record<string, AgentSplit>;
    unattributed: AgentSplit;
  };
  buckets: Record<string, BucketCounts>;
  moves: Record<string, { archived: number; maintenance: number }>;
}

/** Roots, exclusions and aliases (SET-001); paths `~`-expanded, aliases keyed by project path. */
export interface Locations {
  roots: string[];
  exclude: string[];
  aliases: Record<string, string>;
}

export interface Settings {
  config_path: string;
  locations: Locations;
}

/** `check_agent` result (SYN-011). */
export interface AgentCheck {
  agent: string;
  program: string | null;
  ok: boolean;
  model: string | null;
  tokens: number;
  millis: number;
  error: string | null;
}

/** Instruction files that carry the "keep NEXT.md current" rule (LATER-001). */
export interface RuleStatus {
  files: string[];
}

/** A project listed in the weekly digest (LATER-004). */
export interface Neglected {
  project_id: number;
  name: string;
  column: BoardColumn;
  staleness_days: number;
  mess_score: number | null;
}

export interface Synthesis {
  description: string;
  state: string;
  last_goal: string;
  next_steps: string[];
  blockers: string[];
  confidence: "high" | "medium" | "low";
}

export type SynthesisSource = "cache" | "agent" | "next-md";

/** A synthesis available without calling an agent (SYN-010). */
export interface Known {
  synthesis: Synthesis;
  source: SynthesisSource;
  outdated: boolean;
  agent: string | null;
  created_at: string | null;
}

/** One streamed result of a synthesis run. */
export interface SynthesisUpdate {
  run: string;
  path: string;
  ok: boolean;
  synthesis: Synthesis | null;
  source: SynthesisSource | null;
  error: string | null;
}

export interface SynthesisSummary {
  next_md: number;
  cached: number;
  ran: number;
  failed: number;
  batches: number;
  fallbacks: number;
}

/** What a card or the detail panel shows for one project. */
export interface SynthState {
  synthesis: Synthesis | null;
  source: SynthesisSource | null;
  outdated: boolean;
  running: boolean;
  error: string | null;
}

export interface AgentMeter {
  sessions: number;
  /** Input + output + cache writes. */
  tokens: number;
  cache_read: number;
  last_used: string | null;
}

/** `agent_picker` (SYN-009): config default and usage meters. */
export interface PickerInfo {
  default_agent: "ask" | "claude" | "codex";
  meters: {
    claude_code: AgentMeter;
    codex: AgentMeter;
    codex_limit: {
      used_percent: number;
      window_minutes: number | null;
      resets_at: string | null;
      observed_at: string;
      plan_type: string | null;
      expired: boolean;
    } | null;
    claude_limit: ClaudeLimits | null;
  };
}

export interface LimitWindow {
  /** Share used, 0–1. */
  utilization: number;
  resets_at: string | null;
}

/** Claude Code plan usage from a headless run's `rate_limit_event`. */
export interface ClaudeLimits {
  observed_at: string;
  five_hour: LimitWindow | null;
  seven_day: LimitWindow | null;
}

// DEL-001/002: project removal (core `removal::Plan`).
export type RemovalGroup = "folder" | "containers" | "volumes" | "images" | "agent-logs" | "logpose";
export interface RemovalItem {
  id: string;
  group: RemovalGroup;
  label: string;
  detail: string | null;
  bytes: number;
  certainty: "certain" | "probable";
  selected: boolean;
  required: boolean;
  checks: string[];
  action: { kind: string };
}
export interface BranchState {
  name: string;
  upstream: string | null;
  ahead: number;
}
export interface RepoRecovery {
  path: string;
  remotes: { name: string; url: string; web_url: string | null }[];
  branches: BranchState[];
  uncommitted: number;
  stashes: number;
  error: string | null;
}
export interface RemovalPlan {
  project: string;
  items: RemovalItem[];
  build_outputs: { path: string; bytes: number }[];
  repos: RepoRecovery[];
  docker: { state: "available" } | { state: "missing" } | { state: "unreachable"; message: string };
  millis: number;
}
export interface RemovalStep {
  id: string;
  ok: boolean;
  error: string | null;
  bytes: number;
  trashed: boolean;
}
export interface RemovalOutcome {
  steps: RemovalStep[];
  docker_freed: number | null;
}
