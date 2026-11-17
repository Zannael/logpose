// Display text shared by the list, board and detail views.

import type { Agent, BoardColumn, Bucket, MessLabel, ProjectReport } from "./types";

export const bucketText: Record<Bucket, string> = { fresh: "Fresh", cooling: "Cooling", stale: "Stale", dormant: "Dormant" };
export const messText: Record<MessLabel, string> = { clean: "Clean", "tidy-up": "Tidy up", "abandoned-mid-way": "Abandoned mid-way" };
export const agentText: Record<Agent, string> = { "claude-code": "Claude Code", codex: "Codex" };

export const columns: BoardColumn[] = ["now", "next", "paused", "maintenance", "archived"];
export const columnText: Record<BoardColumn, string> = {
  now: "Now",
  next: "Next",
  paused: "Paused",
  maintenance: "Maintenance",
  archived: "Archived",
};
/** Soft WIP limit for Now: a warning appears above it (BOARD-001). */
export const nowLimit = 3;

export function days(p: ProjectReport): string {
  if (!p.staleness) return "–";
  const d = p.staleness.days;
  return d < 1 ? "today" : `${Math.round(d)}d`;
}

export function name(p: ProjectReport): string {
  return p.project.alias ?? p.project.title;
}

/** "3d ago"-style age of an ISO timestamp. */
export function ago(iso: string | null | undefined): string {
  if (!iso) return "–";
  const d = (Date.now() - Date.parse(iso)) / 86_400_000;
  if (d < 1 / 24) return "just now";
  if (d < 1) return `${Math.round(d * 24)}h ago`;
  return `${Math.round(d)}d ago`;
}

/** Full local date and time, for tooltips. */
export function when(iso: string | null | undefined): string {
  return iso ? new Date(iso).toLocaleString() : "";
}

/** `path` relative to `base` when inside it. */
export function relative(path: string, base: string): string {
  return path.startsWith(base + "/") ? path.slice(base.length + 1) : path;
}
