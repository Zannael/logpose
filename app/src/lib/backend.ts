// Calls the Tauri commands. In a plain browser (vite dev without Tauri) it falls back to an
// in-memory mock with synthetic data, so the UI can be previewed and checked without the shell.

import { invoke, isTauri } from "@tauri-apps/api/core";
import type { RemovalOutcome, RemovalPlan, RemovalStep } from "./types";
import type { Agent, AgentCheck, AgentPrograms, BoardColumn, BoardRow, Commit, ClaudeLimits, Known, Locations, Neglected, PickerInfo, RuleStatus, ScanReport, Settings, SetupState, StatsView, SynthesisSummary, SynthesisUpdate } from "./types";
import { mockBackend } from "./mock";

export interface Backend {
  getSetup(): Promise<SetupState>;
  saveRoots(roots: string[]): Promise<SetupState>;
  getSettings(): Promise<Settings>;
  /** Writes roots, exclusions and aliases to config.toml, keeping everything else. */
  saveSettings(locations: Locations): Promise<Settings>;
  /** Scans and persists the result (new projects land in Next). */
  scan(): Promise<ScanReport>;
  board(): Promise<BoardRow[]>;
  /** Moves a card into `column` before card `before` (end of column when null); returns the new board. */
  moveCard(id: number, column: BoardColumn, before: number | null): Promise<BoardRow[]>;
  /** Last first-parent commits of the repository at `path`, newest first. */
  recentCommits(path: string, limit: number): Promise<Commit[]>;
  /** Retroactive stats from local day `from` through `to` (YYYY-MM-DD, inclusive; null = today). */
  stats(projects: { path: string; repos: string[] }[], from: string, to: string | null): Promise<StatsView>;
  /** Runs a tiny synthetic pack through `agent` (spends a few tokens). */
  checkAgent(agent: "claude-code" | "codex"): Promise<AgentCheck>;
  agentRuleStatus(path: string): Promise<RuleStatus>;
  /** Adds or removes the "keep NEXT.md current" rule in the project's CLAUDE.md / AGENTS.md. */
  setAgentRule(path: string, enabled: boolean, agents: Agent[]): Promise<RuleStatus>;
  /** Shows the weekly digest notification when enabled and due; returns what it listed. */
  digestIfDue(): Promise<Neglected[]>;
  digestEnabled(): Promise<boolean>;
  setDigestEnabled(enabled: boolean): Promise<boolean>;
  /** Syntheses available without calling an agent, keyed by project path. */
  knownSyntheses(): Promise<{ path: string; known: Known }[]>;
  /**
   * Synthesize all projects of the last scan (or `paths`) with `agent`; `onUpdate` receives each
   * result as it completes. `force` skips the cache and NEXT.md (per-card refresh).
   */
  synthesize(
    agent: Agent,
    paths: string[] | null,
    force: boolean,
    onUpdate: (u: SynthesisUpdate) => void,
  ): Promise<SynthesisSummary>;
  /** Config default agent, which agents are installed, and usage meters for the startup picker. */
  agentPicker(): Promise<PickerInfo>;
  /** Where the agent CLIs are, and the configured overrides (empty = find automatically). */
  agentPrograms(): Promise<AgentPrograms>;
  /** Saves the overrides to config.toml and returns where they now resolve. */
  setAgentPrograms(claude: string, codex: string): Promise<AgentPrograms>;
  /** More author emails of the user, counted in the commit stats with each repo's user.email. */
  statsEmails(): Promise<string[]>;
  /** Saves them (blank and duplicate entries dropped) and returns the stored list. */
  setStatsEmails(emails: string[]): Promise<string[]>;
  /** Re-applies the suggested column placement to every card; returns the new board. */
  restorePlacement(): Promise<BoardRow[]>;
  /** One-line parking note (empty clears it); returns the board. */
  setParkingNote(id: number, note: string | null): Promise<BoardRow[]>;
  /** Privacy opt-out: keep prompts and agent replies out of this project's summaries. */
  setPrivate(id: number, priv: boolean): Promise<BoardRow[]>;
  /** Opens the project in its editor (PyCharm / WebStorm) or the file manager (DEC-005). */
  openProject(path: string): Promise<{ program: string; kind: string; fell_back: boolean }>;
  /** Tiny Claude call that reads the current plan usage ("Check now"). */
  probeClaudeLimits(): Promise<ClaudeLimits>;
  /** Starts the agent interactively in a terminal in the project, with its summary as the first message. */
  launchAgent(path: string, agent: Agent): Promise<string>;
  trayEnabled(): Promise<boolean>;
  setTrayEnabled(enabled: boolean): Promise<boolean>;
  /** Calls `onRescan` when watched files change or "Rescan now" is chosen in the tray; returns an unsubscribe. */
  onBackgroundRescan(onRescan: () => void): Promise<() => void>;
  /** Everything removing the project would touch, with sizes (changes nothing). */
  removalPlan(path: string): Promise<RemovalPlan>;
  /** Number of branches on the remote, or an error when it cannot be reached. */
  checkRemote(url: string): Promise<number>;
  /** Removes the project: required items plus `selected`, reporting each step. */
  removeProject(path: string, selected: string[], onStep: (s: RemovalStep) => void): Promise<RemovalOutcome>;
  /** Opens a web link in the default browser. */
  openUrl(url: string): Promise<void>;
  /** Native folder picker; `null` when cancelled. */
  pickFolder(): Promise<string | null>;
}

const tauriBackend: Backend = {
  getSetup: () => invoke<SetupState>("get_setup"),
  saveRoots: (roots) => invoke<SetupState>("save_roots", { roots }),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (locations) => invoke<Settings>("save_settings", { locations }),
  scan: () => invoke<ScanReport>("scan"),
  board: () => invoke<BoardRow[]>("board"),
  moveCard: (id, column, before) => invoke<BoardRow[]>("move_card", { id, column, before }),
  recentCommits: (path, limit) => invoke<Commit[]>("recent_commits", { path, limit }),
  stats: (projects, from, to) => invoke<StatsView>("stats", { projects, from, to }),
  checkAgent: (agent) => invoke<AgentCheck>("check_agent", { agent }),
  agentRuleStatus: (path) => invoke<RuleStatus>("agent_rule_status", { path }),
  setAgentRule: (path, enabled, agents) => invoke<RuleStatus>("set_agent_rule", { path, enabled, agents }),
  digestIfDue: () => invoke<Neglected[]>("digest_if_due"),
  digestEnabled: () => invoke<boolean>("digest_enabled"),
  setDigestEnabled: (enabled) => invoke<boolean>("set_digest_enabled", { enabled }),
  knownSyntheses: () => invoke<{ path: string; known: Known }[]>("known_syntheses"),
  synthesize: async (agent, paths, force, onUpdate) => {
    const { listen } = await import("@tauri-apps/api/event");
    const run = crypto.randomUUID();
    const stop = await listen<SynthesisUpdate>("synthesis", (e) => {
      if (e.payload.run === run) onUpdate(e.payload);
    });
    try {
      return await invoke<SynthesisSummary>("synthesize", { run, agent, paths, force });
    } finally {
      stop();
    }
  },
  agentPicker: () => invoke<PickerInfo>("agent_picker"),
  agentPrograms: () => invoke<AgentPrograms>("agent_programs"),
  setAgentPrograms: (claude, codex) => invoke<AgentPrograms>("set_agent_programs", { claude, codex }),
  statsEmails: () => invoke<string[]>("stats_emails"),
  setStatsEmails: (emails) => invoke<string[]>("set_stats_emails", { emails }),
  removalPlan: (path) => invoke<RemovalPlan>("removal_plan", { path }),
  checkRemote: (url) => invoke<number>("check_remote", { url }),
  removeProject: async (path, selected, onStep) => {
    const { Channel } = await import("@tauri-apps/api/core");
    const ch = new Channel<RemovalStep>();
    ch.onmessage = onStep;
    return invoke<RemovalOutcome>("remove_project", { path, selected, onStep: ch });
  },
  openUrl: async (url) => {
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url);
  },
  restorePlacement: () => invoke<BoardRow[]>("restore_placement"),
  setParkingNote: (id, note) => invoke<BoardRow[]>("set_parking_note", { id, note }),
  setPrivate: (id, priv) => invoke<BoardRow[]>("set_private", { id, private: priv }),
  openProject: (path) => invoke("open_project", { path }),
  probeClaudeLimits: () => invoke<ClaudeLimits>("probe_claude_limits"),
  launchAgent: (path, agent) => invoke<string>("launch_agent", { path, agent }),
  trayEnabled: () => invoke<boolean>("tray_enabled"),
  setTrayEnabled: (enabled) => invoke<boolean>("set_tray_enabled", { enabled }),
  onBackgroundRescan: async (onRescan) => {
    const { listen } = await import("@tauri-apps/api/event");
    const stops = await Promise.all([listen("files-changed", onRescan), listen("tray-rescan", onRescan)]);
    return () => stops.forEach((stop) => stop());
  },
  pickFolder: async () => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ directory: true, multiple: false, title: "Choose a folder with projects" });
    return typeof picked === "string" ? picked : null;
  },
};

export const backend: Backend = isTauri() ? tauriBackend : mockBackend;
export const usingMock = !isTauri();
