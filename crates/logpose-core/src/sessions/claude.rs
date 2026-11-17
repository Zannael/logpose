//! Claude Code adapter. Format: `docs/master.md` §4.1.
//!
//! Layout: `<base>/projects/<encoded-cwd>/<sessionId>.jsonl`, base = `$CLAUDE_CONFIG_DIR` or `~/.claude`.
//! Subagent transcripts (`<sessionId>/subagents/*.jsonl`) are not read: they are sidechains.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{
    Agent, FINAL_MESSAGE_MAX_CHARS, Prompt, ScanStats, Session, TokenUsage, for_each_json_line, home_subdir,
    parse_ts, push_prompt, starts_with_tag, truncate_chars,
};

const CONVERSATION_TYPES: [&str; 4] = ["user", "assistant", "attachment", "system"];
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn default_base() -> Option<PathBuf> {
    home_subdir("CLAUDE_CONFIG_DIR", ".claude")
}

/// Read every main session under `<base>/projects`. A missing directory yields no sessions.
pub fn read_sessions(base: &Path) -> (Vec<Session>, ScanStats) {
    let mut stats = ScanStats::default();
    let mut sessions = Vec::new();
    let Ok(projects) = std::fs::read_dir(base.join("projects")) else {
        return (sessions, stats);
    };
    let mut files: Vec<PathBuf> = projects
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| std::fs::read_dir(e.path()).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && p.is_file())
        .collect();
    files.sort();
    // PERF-001: files are parsed in parallel and cached until they change.
    static CACHE: super::FileCache<(Option<Session>, ScanStats)> = super::FileCache::new();
    let parsed = crate::scan::parallel_map(&files, |_, file| {
        CACHE.get_or_parse(file, || {
            let mut stats = ScanStats::default();
            (read_session_file(file, &mut stats), stats)
        })
    });
    for (session, file_stats) in parsed {
        stats.absorb(file_stats);
        sessions.extend(session);
    }
    (sessions, stats)
}

#[derive(Default)]
struct Builder {
    id: Option<String>,
    cwd: Option<String>,
    started: Option<DateTime<Utc>>,
    ended: Option<DateTime<Utc>>,
    prompts: Vec<Prompt>,
    seen_prompts: HashSet<String>,
    final_message: Option<String>,
    custom_title: Option<String>,
    ai_title: Option<String>,
    git_branch: Option<String>,
    tokens: TokenUsage,
    seen_messages: HashSet<(String, String)>,
}

/// Parse one session file. Returns `None` when it has no timestamped conversation records.
pub fn read_session_file(path: &Path, stats: &mut ScanStats) -> Option<Session> {
    let mut b = Builder::default();
    for_each_json_line(path, stats, |rec| b.record(&rec));
    let id =
        b.id.or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default();
    Some(Session {
        agent: Agent::ClaudeCode,
        id,
        cwd: PathBuf::from(b.cwd?),
        started_at: b.started?,
        ended_at: b.ended?,
        prompts: b.prompts,
        final_message: b.final_message,
        title: b.custom_title.or(b.ai_title),
        git_branch: b.git_branch,
        tokens: b.tokens,
        files: vec![path.to_owned()],
    })
}

impl Builder {
    fn record(&mut self, rec: &Value) {
        let kind = rec.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "custom-title" => self.custom_title = str_field(rec, "customTitle").or(self.custom_title.take()),
            "ai-title" => self.ai_title = str_field(rec, "aiTitle").or(self.ai_title.take()),
            _ => {}
        }
        if !CONVERSATION_TYPES.contains(&kind)
            || rec.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            return;
        }
        let ts = parse_ts(rec.get("timestamp"));
        if let Some(ts) = ts {
            self.started = Some(self.started.map_or(ts, |s| s.min(ts)));
            self.ended = Some(self.ended.map_or(ts, |e| e.max(ts)));
        }
        if self.id.is_none() {
            self.id = str_field(rec, "sessionId");
        }
        if self.cwd.is_none() {
            self.cwd = str_field(rec, "cwd");
        }
        if let Some(branch) = str_field(rec, "gitBranch").filter(|b| !b.is_empty()) {
            self.git_branch = Some(branch);
        }
        match kind {
            "user" => self.user(rec, ts),
            "assistant" => self.assistant(rec),
            "attachment" => {
                let att = rec.get("attachment");
                if att.and_then(|a| a.get("type")).and_then(Value::as_str) == Some("queued_command")
                    && let Some(text) = att.and_then(|a| a.get("prompt")).and_then(Value::as_str)
                    && !starts_with_tag(text)
                {
                    let at = parse_ts(att.and_then(|a| a.get("timestamp"))).or(ts);
                    push_prompt(&mut self.prompts, &mut self.seen_prompts, at, text);
                }
            }
            _ => {}
        }
    }

    fn user(&mut self, rec: &Value, ts: Option<DateTime<Utc>>) {
        if rec.get("isMeta").and_then(Value::as_bool) == Some(true) || rec.get("toolUseResult").is_some() {
            return;
        }
        let text = match rec.pointer("/message/content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(blocks)) => {
                if blocks.iter().any(|b| block_type(b) == Some("tool_result")) {
                    return;
                }
                blocks
                    .iter()
                    .filter(|b| block_type(b) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => return,
        };
        if !starts_with_tag(&text) {
            push_prompt(&mut self.prompts, &mut self.seen_prompts, ts, &text);
        }
    }

    fn assistant(&mut self, rec: &Value) {
        let Some(msg) = rec.get("message") else { return };
        if msg.get("model").and_then(Value::as_str) == Some(SYNTHETIC_MODEL) {
            return;
        }
        // One API message is split across records that each repeat the same usage: count it once.
        let key = (
            msg.get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            rec.get("requestId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
        if let Some(usage) = msg.get("usage")
            && (key.0.is_empty() || self.seen_messages.insert(key))
        {
            let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
            self.tokens.add(&TokenUsage {
                input: n("input_tokens"),
                output: n("output_tokens"),
                cache_read: n("cache_read_input_tokens"),
                cache_creation: n("cache_creation_input_tokens"),
            });
        }
        if let Some(Value::Array(blocks)) = msg.get("content") {
            let text: Vec<&str> = blocks
                .iter()
                .filter(|b| block_type(b) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            if !text.is_empty() {
                self.final_message = Some(truncate_chars(text.join("\n").trim(), FINAL_MESSAGE_MAX_CHARS));
            }
        }
    }
}

fn str_field(rec: &Value, key: &str) -> Option<String> {
    rec.get(key).and_then(Value::as_str).map(str::to_string)
}

fn block_type(block: &Value) -> Option<&str> {
    block.get("type").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{fixtures, summarize, utc};

    fn read() -> (Vec<Session>, ScanStats) {
        read_sessions(&fixtures().join("claude"))
    }

    #[test]
    fn main_session_matches_fixture_contract() {
        let (sessions, _) = read();
        let s = sessions.iter().find(|s| s.id.starts_with("1111")).unwrap();
        assert_eq!(s.cwd, PathBuf::from("/home/user/dev/My Project"));
        assert_eq!(s.started_at, utc("2026-10-30T09:00:00.020Z"));
        assert_eq!(s.ended_at, utc("2026-10-30T09:07:00.500Z"));
        let prompts: Vec<_> = s.prompts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(
            prompts,
            [
                "Add a price alert command to the bot",
                "The alert button overlaps the menu, fix the layout",
                "also update the README"
            ]
        );
        assert!(
            s.final_message
                .as_deref()
                .unwrap()
                .starts_with("Done: added /alert")
        );
        assert_eq!(s.title.as_deref(), Some("Price alerts"));
        assert_eq!(s.git_branch.as_deref(), Some("main"));
        assert_eq!(
            s.tokens,
            TokenUsage {
                input: 14,
                output: 320,
                cache_read: 10_600,
                cache_creation: 300
            }
        );
    }

    #[test]
    fn subagents_are_not_sessions_and_bad_lines_are_skipped() {
        let (sessions, stats) = read();
        assert_eq!(sessions.len(), 3);
        assert_eq!(stats.bad_lines, 1);
        let other = sessions.iter().find(|s| s.id.starts_with("3333")).unwrap();
        assert_eq!(other.prompts.len(), 1);
        assert_eq!(
            other.final_message.as_deref(),
            Some("The test depends on wall-clock time.")
        );
        assert!(sessions.iter().all(|s| {
            s.prompts
                .iter()
                .all(|p| p.text != "Find where commands are registered")
        }));
    }

    #[test]
    fn project_summary() {
        let (sessions, _) = read();
        let mine: Vec<&Session> = sessions
            .iter()
            .filter(|s| s.cwd == Path::new("/home/user/dev/My Project"))
            .collect();
        let p = summarize(&mine, utc("2026-10-31T00:00:00Z"), 5).unwrap();
        assert_eq!(p.last_session_at, utc("2026-10-30T09:07:00.500Z"));
        assert_eq!((p.sessions_total, p.sessions_7d), (2, 1));
        assert_eq!(
            p.last_prompts.first().unwrap().text,
            "Set up the Telegram bot skeleton"
        );
    }

    #[test]
    fn missing_base_is_empty() {
        let (sessions, stats) = read_sessions(Path::new("/nonexistent/logpose"));
        assert!(sessions.is_empty());
        assert_eq!(stats, ScanStats::default());
    }
}
