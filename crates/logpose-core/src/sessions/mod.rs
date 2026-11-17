//! Session Reader: zero-token signals from agent session logs (BLUEPRINT §5.3).
//!
//! Each agent has its own adapter because the formats are undocumented and change between
//! versions (see `docs/master.md` §4). Adapters never fail on content: unreadable files and
//! malformed lines are counted in [`ScanStats`] and skipped.

pub mod claude;
pub mod codex;
pub mod merge;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

/// Maximum characters kept for a final agent message.
pub const FINAL_MESSAGE_MAX_CHARS: usize = 1000;

/// Ordered: Claude Code sorts before Codex wherever agents are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
}

/// A real user prompt (injected context, tool results and slash commands excluded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prompt {
    pub at: Option<DateTime<Utc>>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenUsage {
    pub fn add(&mut self, other: &TokenUsage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }

    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }
}

/// One agent session (Claude Code session file, or Codex thread across its rollout files).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub agent: Agent,
    pub id: String,
    /// The session's first working directory; sessions are attributed to this project path.
    pub cwd: PathBuf,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub prompts: Vec<Prompt>,
    pub final_message: Option<String>,
    pub title: Option<String>,
    pub git_branch: Option<String>,
    pub tokens: TokenUsage,
    /// The log files this session was read from (one for Claude Code, every rollout file of a Codex
    /// thread), so project removal can offer them (DEL-001).
    #[serde(skip)]
    pub files: Vec<PathBuf>,
}

/// Counters for defensive parsing; surfaced in the UI as "n lines skipped".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ScanStats {
    pub files: usize,
    pub unreadable_files: usize,
    pub bad_lines: usize,
}

impl ScanStats {
    pub fn absorb(&mut self, other: ScanStats) {
        self.files += other.files;
        self.unreadable_files += other.unreadable_files;
        self.bad_lines += other.bad_lines;
    }
}

/// Parsed log files kept for the life of the process, keyed by path and reused while the file's
/// size and modification time are unchanged (PERF-001). Session files only grow or are replaced,
/// so a rescan re-reads just the active ones.
pub(crate) struct FileCache<T>(std::sync::OnceLock<std::sync::Mutex<FileCacheMap<T>>>);
type FileCacheMap<T> = std::collections::HashMap<PathBuf, (u64, Option<std::time::SystemTime>, T)>;

impl<T: Clone> FileCache<T> {
    pub(crate) const fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }

    pub(crate) fn get_or_parse(&self, path: &Path, parse: impl FnOnce() -> T) -> T {
        let map = self.0.get_or_init(Default::default);
        let key = std::fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()));
        if let Some((len, modified)) = key
            && let Some((l, m, v)) = map.lock().ok().and_then(|c| c.get(path).cloned())
            && l == len
            && m == modified
        {
            return v;
        }
        let v = parse();
        if let Some((len, modified)) = key
            && let Ok(mut c) = map.lock()
        {
            c.insert(path.to_owned(), (len, modified, v.clone()));
        }
        v
    }
}

/// Per-project view of all sessions for that project (any agent).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectActivity {
    pub last_session_at: DateTime<Utc>,
    pub last_agent: Agent,
    pub agents: Vec<Agent>,
    pub sessions_7d: usize,
    pub sessions_30d: usize,
    pub sessions_total: usize,
    /// Most recent prompts, oldest first, taken from the latest sessions.
    pub last_prompts: Vec<Prompt>,
    pub final_message: Option<String>,
    pub title: Option<String>,
    pub tokens: TokenUsage,
}

/// Aggregate sessions of one project. Returns `None` when `sessions` is empty.
pub fn summarize(sessions: &[&Session], now: DateTime<Utc>, max_prompts: usize) -> Option<ProjectActivity> {
    let mut ordered: Vec<&Session> = sessions.to_vec();
    ordered.sort_by_key(|s| s.ended_at);
    let latest = *ordered.last()?;

    let mut agents: Vec<Agent> = Vec::new();
    let mut tokens = TokenUsage::default();
    for s in &ordered {
        if !agents.contains(&s.agent) {
            agents.push(s.agent);
        }
        tokens.add(&s.tokens);
    }
    agents.sort();
    let within = |days: i64| {
        ordered
            .iter()
            .filter(|s| now - s.ended_at <= Duration::days(days))
            .count()
    };

    let mut last_prompts: Vec<Prompt> = Vec::new();
    for s in ordered.iter().rev() {
        for p in s.prompts.iter().rev() {
            if last_prompts.len() == max_prompts {
                break;
            }
            last_prompts.push(p.clone());
        }
    }
    last_prompts.reverse();

    let final_message = ordered.iter().rev().find_map(|s| s.final_message.clone());
    Some(ProjectActivity {
        last_session_at: latest.ended_at,
        last_agent: latest.agent,
        agents,
        sessions_7d: within(7),
        sessions_30d: within(30),
        sessions_total: ordered.len(),
        last_prompts,
        final_message,
        title: latest.title.clone(),
        tokens,
    })
}

// ---- shared helpers for adapters ----

pub(crate) fn parse_ts(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    let s = value?.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

pub(crate) fn epoch_secs(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(value?.as_i64()?, 0)
}

/// Read a JSONL file line by line, calling `f` for each parsed JSON value.
/// Returns false when the file cannot be opened.
pub(crate) fn for_each_json_line(
    path: &Path,
    stats: &mut ScanStats,
    mut f: impl FnMut(serde_json::Value),
) -> bool {
    let Ok(file) = File::open(path) else {
        stats.unreadable_files += 1;
        return false;
    };
    stats.files += 1;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            stats.bad_lines += 1;
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(value) => f(value),
            Err(_) => stats.bad_lines += 1,
        }
    }
    true
}

/// True when text is injected context rather than something the user typed (`<tag>…`).
pub(crate) fn starts_with_tag(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<')
        && t[1..].chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && t.find('>').is_some_and(|end| {
            t[1..end]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((idx, _)) => format!("{}…", &text[..idx]),
        None => text.to_string(),
    }
}

/// Push a prompt unless the same text was already recorded in this session.
pub(crate) fn push_prompt(
    prompts: &mut Vec<Prompt>,
    seen: &mut HashSet<String>,
    at: Option<DateTime<Utc>>,
    text: &str,
) {
    let text = text.trim();
    if !text.is_empty() && seen.insert(text.to_string()) {
        prompts.push(Prompt {
            at,
            text: text.to_string(),
        });
    }
}

pub(crate) fn home_subdir(env_var: &str, default: &str) -> Option<PathBuf> {
    crate::platform::Platform::current().home_subdir(env_var, default)
}

#[cfg(test)]
pub(crate) fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

#[cfg(test)]
pub(crate) fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_detection() {
        assert!(starts_with_tag("  <command-name>/x</command-name>"));
        assert!(starts_with_tag("<environment_context>"));
        assert!(!starts_with_tag("<3 this works"));
        assert!(!starts_with_tag("a < b > c"));
        assert!(!starts_with_tag("plain"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate_chars("àèìòù", 3), "àèì…");
        assert_eq!(truncate_chars("abc", 3), "abc");
    }

    fn session(agent: Agent, end: &str, prompts: &[&str], fin: Option<&str>) -> Session {
        Session {
            agent,
            id: end.into(),
            cwd: "/p".into(),
            started_at: utc(end),
            ended_at: utc(end),
            prompts: prompts
                .iter()
                .map(|t| Prompt {
                    at: None,
                    text: t.to_string(),
                })
                .collect(),
            final_message: fin.map(str::to_string),
            title: Some(format!("t{end}")),
            git_branch: None,
            tokens: TokenUsage {
                input: 1,
                output: 2,
                cache_read: 0,
                cache_creation: 0,
            },
            files: vec![],
        }
    }

    #[test]
    fn agent_order_is_fixed() {
        let codex_first = session(Agent::Codex, "2026-10-01T00:00:00Z", &[], None);
        let claude_later = session(Agent::ClaudeCode, "2026-10-20T00:00:00Z", &[], None);
        let s = summarize(&[&codex_first, &claude_later], utc("2026-10-31T00:00:00Z"), 3).unwrap();
        assert_eq!(s.agents, vec![Agent::ClaudeCode, Agent::Codex]);
    }

    #[test]
    fn summarize_orders_and_counts() {
        let a = session(
            Agent::ClaudeCode,
            "2026-10-01T00:00:00Z",
            &["a1", "a2"],
            Some("fa"),
        );
        let b = session(Agent::Codex, "2026-10-28T00:00:00Z", &["b1", "b2", "b3"], None);
        let now = utc("2026-10-31T00:00:00Z");
        let s = summarize(&[&b, &a], now, 4).unwrap();
        assert_eq!(s.last_agent, Agent::Codex);
        assert_eq!(s.agents, vec![Agent::ClaudeCode, Agent::Codex]);
        assert_eq!((s.sessions_7d, s.sessions_30d, s.sessions_total), (1, 2, 2));
        let texts: Vec<_> = s.last_prompts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["a2", "b1", "b2", "b3"]);
        assert_eq!(s.final_message.as_deref(), Some("fa"));
        assert_eq!(s.tokens.total(), 6);
        assert!(summarize(&[], now, 3).is_none());
    }
}

#[cfg(test)]
mod file_cache_tests {
    use super::*;

    #[test]
    fn file_cache_reparses_only_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "a").unwrap();
        let cache: FileCache<String> = FileCache::new();
        let calls = std::cell::Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            std::fs::read_to_string(&path).unwrap()
        };
        assert_eq!(cache.get_or_parse(&path, read), "a");
        assert_eq!(cache.get_or_parse(&path, read), "a");
        assert_eq!(calls.get(), 1, "unchanged file comes from the cache");
        std::fs::write(&path, "ab").unwrap();
        assert_eq!(
            cache.get_or_parse(&path, read),
            "ab",
            "a grown file is parsed again"
        );
        assert_eq!(calls.get(), 2);
        std::fs::remove_file(&path).unwrap();
        cache.get_or_parse(&path, String::new);
        assert_eq!(
            cache.get_or_parse(&path, || "gone".into()),
            "gone",
            "a missing file is never cached"
        );
    }
}
