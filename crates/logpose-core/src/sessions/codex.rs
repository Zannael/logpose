//! Codex adapter. Format: `docs/master.md` §4.2.
//!
//! Layout: `<base>/sessions/YYYY/MM/DD/rollout-*.jsonl` and `<base>/archived_sessions/rollout-*.jsonl`,
//! base = `$CODEX_HOME` or `~/.codex`. A thread can span several rollout files, so files are grouped
//! by `session_meta.id`. Guardian reviews and spawned subagents (`parent_thread_id`) are skipped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use super::{
    Agent, FINAL_MESSAGE_MAX_CHARS, Prompt, ScanStats, Session, TokenUsage, epoch_secs, for_each_json_line,
    home_subdir, parse_ts, push_prompt, starts_with_tag, truncate_chars,
};

/// Prefixes of injected context in `role=user` messages (besides `<tag>` blocks).
const INJECTED_PREFIXES: [&str; 2] = ["# AGENTS.md", "# Files"];

/// `$CODEX_HOME`, else `~/.codex`.
pub fn default_base() -> Option<PathBuf> {
    home_subdir("CODEX_HOME", ".codex")
}

/// Latest account-level rate-limit snapshot from `token_count` events.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RateLimit {
    pub observed_at: DateTime<Utc>,
    pub limit_id: Option<String>,
    pub plan_type: Option<String>,
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default)]
pub struct CodexScan {
    pub sessions: Vec<Session>,
    pub rate_limit: Option<RateLimit>,
    pub stats: ScanStats,
}

/// One rollout file, parsed.
#[derive(Clone, Default)]
struct FilePart {
    path: PathBuf,
    thread_id: Option<String>,
    cwd: Option<String>,
    is_subagent: bool,
    started: Option<DateTime<Utc>>,
    ended: Option<DateTime<Utc>>,
    prompts: Vec<Prompt>,
    final_from_task: Option<String>,
    final_from_answer: Option<String>,
    final_from_event: Option<String>,
    git_branch: Option<String>,
    first_total: Option<TokenUsage>,
    last_total: Option<TokenUsage>,
    rate_limit: Option<RateLimit>,
}

pub fn read_sessions(base: &Path) -> CodexScan {
    let mut scan = CodexScan::default();
    let mut files = Vec::new();
    collect_rollouts(&base.join("sessions"), &mut files);
    collect_rollouts(&base.join("archived_sessions"), &mut files);
    files.sort();

    // PERF-001: files are parsed in parallel and cached until they change.
    static CACHE: super::FileCache<(FilePart, ScanStats)> = super::FileCache::new();
    let parsed = crate::scan::parallel_map(&files, |_, file| {
        CACHE.get_or_parse(file, || {
            let mut stats = ScanStats::default();
            (read_file(file, &mut stats), stats)
        })
    });
    let mut threads: BTreeMap<String, Vec<FilePart>> = BTreeMap::new();
    for (part, file_stats) in parsed {
        scan.stats.absorb(file_stats);
        if let Some(rl) = &part.rate_limit
            && scan
                .rate_limit
                .as_ref()
                .is_none_or(|cur| rl.observed_at > cur.observed_at)
        {
            scan.rate_limit = Some(rl.clone());
        }
        if part.is_subagent {
            continue;
        }
        if let Some(id) = part.thread_id.clone() {
            threads.entry(id).or_default().push(part);
        }
    }

    let titles = read_titles(&base.join("session_index.jsonl"), &mut scan.stats);
    for (id, mut parts) in threads {
        parts.sort_by_key(|p| p.started);
        if let Some(session) = merge(id, parts, &titles) {
            scan.sessions.push(session);
        }
    }
    scan
}

fn collect_rollouts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_rollouts(&path, out),
            Ok(t) if t.is_file() => {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
}

fn read_file(path: &Path, stats: &mut ScanStats) -> FilePart {
    let mut part = FilePart {
        path: path.to_owned(),
        ..FilePart::default()
    };
    let mut seen = HashSet::new();
    for_each_json_line(path, stats, |rec| {
        let ts = parse_ts(rec.get("timestamp"));
        if let Some(ts) = ts {
            part.started = Some(part.started.map_or(ts, |s| s.min(ts)));
            part.ended = Some(part.ended.map_or(ts, |e| e.max(ts)));
        }
        let kind = rec.get("type").and_then(Value::as_str).unwrap_or_default();
        let Some(p) = rec.get("payload") else { return };
        let ptype = p.get("type").and_then(Value::as_str).unwrap_or_default();
        match (kind, ptype) {
            ("session_meta", _) if part.thread_id.is_none() => {
                part.thread_id = p.get("id").and_then(Value::as_str).map(str::to_string);
                part.cwd = p.get("cwd").and_then(Value::as_str).map(str::to_string);
                part.is_subagent = p.get("parent_thread_id").is_some()
                    || matches!(
                        p.get("thread_source").and_then(Value::as_str),
                        Some("guardian_review" | "subagent")
                    );
                part.git_branch = p
                    .pointer("/git/branch")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            ("response_item", "message") => {
                let role = p.get("role").and_then(Value::as_str);
                let text = joined_text(p.get("content"));
                if role == Some("user") && !is_injected(&text) {
                    push_prompt(&mut part.prompts, &mut seen, ts, &text);
                } else if role == Some("assistant")
                    && p.get("phase").and_then(Value::as_str) == Some("final_answer")
                    && !text.trim().is_empty()
                {
                    part.final_from_answer = Some(text);
                }
            }
            ("event_msg", "user_message") => {
                if let Some(text) = p.get("message").and_then(Value::as_str)
                    && !is_injected(text)
                {
                    push_prompt(&mut part.prompts, &mut seen, ts, text);
                }
            }
            ("event_msg", "agent_message") => {
                part.final_from_event = p.get("message").and_then(Value::as_str).map(str::to_string);
            }
            ("event_msg", "task_complete") => {
                if let Some(text) = p.get("last_agent_message").and_then(Value::as_str) {
                    part.final_from_task = Some(text.to_string());
                }
            }
            ("event_msg", "token_count") => {
                if let Some(total) = p.pointer("/info/total_token_usage").filter(|v| v.is_object()) {
                    let n = |k: &str| total.get(k).and_then(Value::as_u64).unwrap_or(0);
                    let cached = n("cached_input_tokens");
                    let usage = TokenUsage {
                        // `input_tokens` includes cached input; keep the buckets disjoint like Claude's.
                        input: n("input_tokens").saturating_sub(cached),
                        output: n("output_tokens"),
                        cache_read: cached,
                        cache_creation: n("cache_write_input_tokens"),
                    };
                    part.first_total.get_or_insert(usage);
                    part.last_total = Some(usage);
                }
                if let (Some(rl), Some(at)) = (p.get("rate_limits").filter(|v| v.is_object()), ts)
                    && let Some(used) = rl.pointer("/primary/used_percent").and_then(Value::as_f64)
                {
                    part.rate_limit = Some(RateLimit {
                        observed_at: at,
                        limit_id: rl.get("limit_id").and_then(Value::as_str).map(str::to_string),
                        plan_type: rl.get("plan_type").and_then(Value::as_str).map(str::to_string),
                        used_percent: used,
                        window_minutes: rl.pointer("/primary/window_minutes").and_then(Value::as_u64),
                        resets_at: epoch_secs(rl.pointer("/primary/resets_at")),
                    });
                }
            }
            _ => {}
        }
    });
    part
}

fn merge(id: String, parts: Vec<FilePart>, titles: &HashMap<String, String>) -> Option<Session> {
    let cwd = parts.iter().find_map(|p| p.cwd.clone())?;
    let started = parts.iter().filter_map(|p| p.started).min()?;
    let ended = parts.iter().filter_map(|p| p.ended).max()?;
    let mut prompts = Vec::new();
    let mut seen = HashSet::new();
    let mut tokens = TokenUsage::default();
    let mut prev_last: Option<TokenUsage> = None;
    let mut final_message = None;
    let mut git_branch = None;
    for part in &parts {
        for p in &part.prompts {
            push_prompt(&mut prompts, &mut seen, p.at, &p.text);
        }
        // Continuation files either keep the running total or restart it (both observed).
        if let (Some(first), Some(last)) = (part.first_total, part.last_total) {
            match prev_last {
                Some(prev) if first.total() >= prev.total() => tokens.add(&diff(&last, &prev)),
                _ => tokens.add(&last),
            }
            prev_last = Some(last);
        }
        if let Some(text) = part
            .final_from_task
            .clone()
            .or(part.final_from_answer.clone())
            .or(part.final_from_event.clone())
        {
            final_message = Some(truncate_chars(text.trim(), FINAL_MESSAGE_MAX_CHARS));
        }
        git_branch = part.git_branch.clone().or(git_branch);
    }
    Some(Session {
        agent: Agent::Codex,
        title: titles.get(&id).cloned(),
        id,
        cwd: PathBuf::from(cwd),
        started_at: started,
        ended_at: ended,
        prompts,
        final_message,
        git_branch,
        tokens,
        files: parts.iter().map(|p| p.path.clone()).collect(),
    })
}

fn diff(a: &TokenUsage, b: &TokenUsage) -> TokenUsage {
    TokenUsage {
        input: a.input.saturating_sub(b.input),
        output: a.output.saturating_sub(b.output),
        cache_read: a.cache_read.saturating_sub(b.cache_read),
        cache_creation: a.cache_creation.saturating_sub(b.cache_creation),
    }
}

fn read_titles(path: &Path, stats: &mut ScanStats) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    if path.is_file() {
        for_each_json_line(path, stats, |rec| {
            if let (Some(id), Some(name)) = (
                rec.get("id").and_then(Value::as_str),
                rec.get("thread_name").and_then(Value::as_str),
            ) {
                titles.insert(id.to_string(), name.to_string());
            }
        });
    }
    titles
}

fn joined_text(content: Option<&Value>) -> String {
    let Some(Value::Array(blocks)) = content else {
        return String::new();
    };
    blocks
        .iter()
        .filter(|b| {
            matches!(
                b.get("type").and_then(Value::as_str),
                Some("input_text" | "output_text")
            )
        })
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_injected(text: &str) -> bool {
    let t = text.trim_start();
    t.is_empty() || starts_with_tag(t) || INJECTED_PREFIXES.iter().any(|p| t.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{fixtures, utc};

    fn scan() -> CodexScan {
        read_sessions(&fixtures().join("codex"))
    }

    #[test]
    fn thread_spanning_two_files() {
        let scan = scan();
        let s = scan.sessions.iter().find(|s| s.id.ends_with("0001")).unwrap();
        assert_eq!(s.cwd, PathBuf::from("/home/user/dev/My Project"));
        assert_eq!(s.started_at, utc("2026-10-30T10:00:00.000Z"));
        assert_eq!(s.ended_at, utc("2026-10-30T15:00:31.000Z"));
        let prompts: Vec<_> = s.prompts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(
            prompts,
            [
                "Wire the WireGuard tunnel to the agent loop",
                "Now add the handshake retry with backoff"
            ]
        );
        assert_eq!(
            s.final_message.as_deref(),
            Some("Retry with exponential backoff added.")
        );
        assert_eq!(s.title.as_deref(), Some("WireGuard agent loop"));
        assert_eq!(s.git_branch.as_deref(), Some("main"));
        // Running total continued from 12 800 to 32 100 tokens.
        assert_eq!(s.tokens.total(), 32_100);
    }

    #[test]
    fn guardian_ignored_and_old_format_deduplicated() {
        let scan = scan();
        assert_eq!(scan.sessions.len(), 2, "guardian review must be skipped");
        assert!(
            scan.sessions
                .iter()
                .all(|s| s.ended_at < utc("2026-10-31T00:00:00Z"))
        );
        let old = scan.sessions.iter().find(|s| s.id.ends_with("0003")).unwrap();
        assert_eq!(old.prompts.len(), 1);
        assert_eq!(old.final_message, None);
        assert_eq!(old.ended_at, utc("2026-09-13T16:01:00.000Z"));
    }

    #[test]
    fn latest_rate_limit() {
        let rl = scan().rate_limit.unwrap();
        assert_eq!(rl.used_percent, 34.5);
        assert_eq!(rl.window_minutes, Some(10080));
        assert_eq!(rl.resets_at, DateTime::from_timestamp(1_793_900_000, 0));
        assert_eq!(rl.plan_type.as_deref(), Some("plus"));
    }

    #[test]
    fn restarted_totals_are_added() {
        let a = TokenUsage {
            input: 100,
            ..Default::default()
        };
        let b = TokenUsage {
            input: 30,
            ..Default::default()
        };
        let part = |t| FilePart {
            thread_id: Some("t".into()),
            cwd: Some("/p".into()),
            started: Some(utc("2026-01-01T00:00:00Z")),
            ended: Some(utc("2026-01-01T00:00:00Z")),
            first_total: Some(t),
            last_total: Some(t),
            ..Default::default()
        };
        let s = merge("t".into(), vec![part(a), part(b)], &HashMap::new()).unwrap();
        assert_eq!(s.tokens.input, 130);
    }
}
