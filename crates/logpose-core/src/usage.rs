//! Usage meters for the agent picker (SYN-009, BLUEPRINT §5.6). Codex's latest rate-limit snapshot
//! comes from its logs. Claude Code keeps no quota locally, but every headless `stream-json` run
//! reports it (`rate_limit_event`); Logpose stores the latest one under [`CLAUDE_LIMITS_KEY`]. Both are
//! exact as of when they were observed. Recent token usage from the logs is an estimate.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::runner::ClaudeLimits;
use crate::sessions::codex::RateLimit;
use crate::store::Store;

/// Store setting holding the latest [`ClaudeLimits`] as JSON.
pub const CLAUDE_LIMITS_KEY: &str = "claude.limits";

pub fn save_claude_limits(store: &Store, l: &ClaudeLimits) -> rusqlite::Result<()> {
    store.set_setting(CLAUDE_LIMITS_KEY, &serde_json::to_string(l).unwrap_or_default())
}

pub fn load_claude_limits(store: &Store) -> rusqlite::Result<Option<ClaudeLimits>> {
    Ok(store
        .setting(CLAUDE_LIMITS_KEY)?
        .and_then(|j| serde_json::from_str(&j).ok()))
}
use crate::sessions::{Agent, Session};

pub const RECENT_DAYS: i64 = 7;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodexLimit {
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    pub plan_type: Option<String>,
    /// The window reset after the snapshot was taken, so `used_percent` no longer applies.
    pub expired: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AgentMeter {
    /// Sessions that ended in the last [`RECENT_DAYS`] days.
    pub sessions: usize,
    /// Their input + output + cache-write tokens. An estimate of spend, not of quota.
    pub tokens: u64,
    /// Their cache reads, kept apart: they dominate long sessions (~10⁹ a week here) and are billed
    /// at a fraction of the input price, so adding them would make the meter meaningless.
    pub cache_read: u64,
    pub last_used: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UsageMeters {
    pub claude_code: AgentMeter,
    pub codex: AgentMeter,
    /// Exact Codex plan usage when a snapshot exists.
    pub codex_limit: Option<CodexLimit>,
    /// Exact Claude Code plan usage from the latest Logpose call to Claude (5-hour and weekly).
    pub claude_limit: Option<ClaudeLimits>,
}

pub fn meters(
    sessions: &[Session],
    rate_limit: Option<&RateLimit>,
    claude_limit: Option<ClaudeLimits>,
    now: DateTime<Utc>,
) -> UsageMeters {
    let mut m = UsageMeters {
        claude_limit,
        ..UsageMeters::default()
    };
    for s in sessions {
        let meter = match s.agent {
            Agent::ClaudeCode => &mut m.claude_code,
            Agent::Codex => &mut m.codex,
        };
        meter.last_used = meter.last_used.max(Some(s.ended_at));
        if now - s.ended_at <= Duration::days(RECENT_DAYS) {
            meter.sessions += 1;
            meter.tokens += s.tokens.input + s.tokens.output + s.tokens.cache_creation;
            meter.cache_read += s.tokens.cache_read;
        }
    }
    m.codex_limit = rate_limit.map(|r| CodexLimit {
        used_percent: r.used_percent,
        window_minutes: r.window_minutes,
        resets_at: r.resets_at,
        observed_at: r.observed_at,
        plan_type: r.plan_type.clone(),
        expired: r.resets_at.is_some_and(|t| t <= now),
    });
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{TokenUsage, utc};

    fn session(agent: Agent, ended: &str, tokens: u64) -> Session {
        Session {
            agent,
            id: ended.into(),
            cwd: "/p".into(),
            started_at: utc(ended),
            ended_at: utc(ended),
            prompts: vec![],
            final_message: None,
            title: None,
            git_branch: None,
            tokens: TokenUsage {
                input: tokens,
                cache_read: tokens * 100,
                ..Default::default()
            },
            files: vec![],
        }
    }

    #[test]
    fn recent_tokens_per_agent_and_codex_limit() {
        let now = utc("2026-11-03T12:00:00Z");
        let sessions = [
            session(Agent::ClaudeCode, "2026-11-02T10:00:00Z", 1_000),
            session(Agent::ClaudeCode, "2026-10-20T10:00:00Z", 9_999), // older than 7 days
            session(Agent::Codex, "2026-11-03T09:00:00Z", 20_000),
        ];
        let limit = RateLimit {
            observed_at: utc("2026-11-03T09:00:00Z"),
            limit_id: None,
            plan_type: Some("plus".into()),
            used_percent: 34.5,
            window_minutes: Some(10_080),
            resets_at: Some(utc("2026-11-06T00:00:00Z")),
        };
        let m = meters(&sessions, Some(&limit), None, now);
        assert_eq!(
            (
                m.claude_code.sessions,
                m.claude_code.tokens,
                m.claude_code.cache_read
            ),
            (1, 1_000, 100_000)
        );
        assert_eq!(m.claude_code.last_used, Some(utc("2026-11-02T10:00:00Z")));
        assert_eq!((m.codex.sessions, m.codex.tokens), (1, 20_000));
        let l = m.codex_limit.unwrap();
        assert_eq!((l.used_percent, l.expired), (34.5, false));

        // After the reset time the snapshot no longer applies.
        let later = meters(&sessions, Some(&limit), None, utc("2026-11-07T00:00:00Z"));
        assert!(later.codex_limit.unwrap().expired);
        assert!(meters(&[], None, None, now).codex_limit.is_none());
    }

    #[test]
    fn claude_limits_round_trip_through_the_store() {
        let store = Store::open_in_memory().unwrap();
        assert!(load_claude_limits(&store).unwrap().is_none());
        let l = ClaudeLimits {
            observed_at: utc("2026-11-03T09:00:00Z"),
            five_hour: Some(crate::runner::LimitWindow {
                utilization: 0.43,
                resets_at: Some(utc("2026-11-03T10:50:00Z")),
            }),
            seven_day: None,
        };
        save_claude_limits(&store, &l).unwrap();
        assert_eq!(load_claude_limits(&store).unwrap(), Some(l.clone()));
        let m = meters(&[], None, Some(l), utc("2026-11-03T09:30:00Z"));
        assert_eq!(m.claude_limit.unwrap().five_hour.unwrap().utilization, 0.43);
    }
}
