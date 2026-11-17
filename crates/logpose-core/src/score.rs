//! Staleness and mess scoring (BLUEPRINT §6, SCORE-001/SCORE-002).

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::ScoreWeights;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StalenessBucket {
    /// < 3 days
    Fresh,
    /// 3–14 days
    Cooling,
    /// 14–60 days
    Stale,
    /// > 60 days
    Dormant,
}

impl StalenessBucket {
    pub fn from_days(days: f64) -> Self {
        if days < 3.0 {
            Self::Fresh
        } else if days < 14.0 {
            Self::Cooling
        } else if days <= 60.0 {
            Self::Stale
        } else {
            Self::Dormant
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Staleness {
    pub last_activity: DateTime<Utc>,
    pub days: f64,
    pub bucket: StalenessBucket,
}

/// Days since the latest of commit, file change and agent session. `None` when nothing is known.
pub fn staleness(
    last_commit: Option<DateTime<Utc>>,
    last_mtime: Option<DateTime<Utc>>,
    last_session: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<Staleness> {
    let last = [last_commit, last_mtime, last_session]
        .into_iter()
        .flatten()
        .max()?;
    let days = ((now - last).num_seconds() as f64 / 86_400.0).max(0.0);
    Some(Staleness {
        last_activity: last,
        days,
        bucket: StalenessBucket::from_days(days),
    })
}

/// Inputs of the mess score; all from the git scanner (GIT-001..004).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MessInputs {
    /// Merge/rebase/cherry-pick in progress, or conflicted files.
    pub operation_in_progress: bool,
    pub uncommitted_files: usize,
    pub oldest_uncommitted_days: Option<f64>,
    pub unpushed_commits: usize,
    pub stash_count: usize,
    pub stale_branches: usize,
    pub no_upstream: bool,
    /// Days since the project's last activity (staleness); `None` when unknown.
    pub days_inactive: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessLabel {
    Clean,
    TidyUp,
    AbandonedMidWay,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Mess {
    /// 0–100.
    pub score: f64,
    pub label: MessLabel,
}

/// Threshold after which an uncommitted change marks the project "abandoned mid-way".
pub const ABANDONED_AFTER_DAYS: f64 = 14.0;

/// Inactivity (days) after which old uncommitted work marks a project abandoned (= start of Cooling).
pub const QUIET_AFTER_DAYS: f64 = 3.0;

pub fn mess(i: &MessInputs, w: &ScoreWeights) -> Mess {
    let frac = |n: f64, cap: f64| (n / cap).min(1.0);
    let score = w.operation_in_progress * f64::from(u8::from(i.operation_in_progress))
        + w.uncommitted_files * frac(i.uncommitted_files as f64, 20.0)
        + w.oldest_uncommitted * frac(i.oldest_uncommitted_days.unwrap_or(0.0), 14.0)
        + w.unpushed_commits * frac(i.unpushed_commits as f64, 10.0)
        + w.stashes * frac(i.stash_count as f64, 3.0)
        + w.stale_branches * frac(i.stale_branches as f64, 5.0)
        + w.no_upstream * f64::from(u8::from(i.no_upstream));
    let score = score.clamp(0.0, 100.0);
    // Old leftovers only mean "abandoned" once the project has gone quiet (SCORE-003).
    let quiet = i.days_inactive.is_none_or(|d| d >= QUIET_AFTER_DAYS);
    let abandoned = quiet
        && i.oldest_uncommitted_days
            .is_some_and(|d| d > ABANDONED_AFTER_DAYS);
    let label = if score > 50.0 || abandoned {
        MessLabel::AbandonedMidWay
    } else if score >= 15.0 {
        MessLabel::TidyUp
    } else {
        MessLabel::Clean
    };
    Mess { score, label }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn bucket_boundaries() {
        let cases = [
            (0.0, StalenessBucket::Fresh),
            (2.99, StalenessBucket::Fresh),
            (3.0, StalenessBucket::Cooling),
            (13.99, StalenessBucket::Cooling),
            (14.0, StalenessBucket::Stale),
            (60.0, StalenessBucket::Stale),
            (60.01, StalenessBucket::Dormant),
        ];
        for (days, bucket) in cases {
            assert_eq!(StalenessBucket::from_days(days), bucket, "{days}");
        }
    }

    #[test]
    fn staleness_uses_latest_signal() {
        let now = at("2026-11-01T00:00:00Z");
        let s = staleness(
            Some(at("2026-09-01T00:00:00Z")),
            None,
            Some(at("2026-10-27T00:00:00Z")),
            now,
        )
        .unwrap();
        assert_eq!(
            (s.days, s.bucket, s.last_activity),
            (5.0, StalenessBucket::Cooling, at("2026-10-27T00:00:00Z"))
        );
        assert!(staleness(None, None, None, now).is_none());
        // Clock skew (future mtime) never yields negative days.
        assert_eq!(
            staleness(None, Some(at("2026-11-02T00:00:00Z")), None, now)
                .unwrap()
                .days,
            0.0
        );
    }

    #[test]
    fn each_term_with_default_weights() {
        let w = ScoreWeights::default();
        let only = |f: &dyn Fn(&mut MessInputs)| {
            let mut i = MessInputs::default();
            f(&mut i);
            mess(&i, &w).score
        };
        assert_eq!(only(&|_| {}), 0.0);
        assert_eq!(only(&|i| i.operation_in_progress = true), 25.0);
        assert_eq!(only(&|i| i.uncommitted_files = 10), 10.0);
        assert_eq!(only(&|i| i.uncommitted_files = 500), 20.0);
        assert_eq!(only(&|i| i.oldest_uncommitted_days = Some(7.0)), 7.5);
        assert_eq!(only(&|i| i.unpushed_commits = 5), 7.5);
        assert_eq!(only(&|i| i.stash_count = 3), 10.0);
        assert_eq!(only(&|i| i.stale_branches = 1), 2.0);
        assert_eq!(only(&|i| i.no_upstream = true), 5.0);
        let max = MessInputs {
            operation_in_progress: true,
            uncommitted_files: 99,
            oldest_uncommitted_days: Some(99.0),
            unpushed_commits: 99,
            stash_count: 99,
            stale_branches: 99,
            no_upstream: true,
            days_inactive: None,
        };
        assert_eq!(mess(&max, &w).score, 100.0);
    }

    #[test]
    fn old_leftovers_in_active_projects_are_not_abandoned() {
        let w = ScoreWeights::default();
        let base = MessInputs {
            uncommitted_files: 1,
            oldest_uncommitted_days: Some(20.0),
            ..Default::default()
        };
        let fresh = MessInputs {
            days_inactive: Some(1.0),
            ..base.clone()
        };
        assert_eq!(
            mess(&fresh, &w).label,
            MessLabel::TidyUp,
            "score 16 → Tidy up, not abandoned"
        );
        let cooling = MessInputs {
            days_inactive: Some(3.0),
            ..base.clone()
        };
        assert_eq!(mess(&cooling, &w).label, MessLabel::AbandonedMidWay);
        assert_eq!(
            mess(&base, &w).label,
            MessLabel::AbandonedMidWay,
            "unknown activity counts as quiet"
        );
    }

    #[test]
    fn label_boundaries_and_custom_weights() {
        let w = ScoreWeights::default();
        let label = |i: MessInputs| mess(&i, &w).label;
        assert_eq!(
            label(MessInputs {
                no_upstream: true,
                ..Default::default()
            }),
            MessLabel::Clean
        );
        assert_eq!(
            label(MessInputs {
                stash_count: 3,
                no_upstream: true,
                ..Default::default()
            }),
            MessLabel::TidyUp
        );
        assert_eq!(
            label(MessInputs {
                operation_in_progress: true,
                uncommitted_files: 20,
                unpushed_commits: 10,
                ..Default::default()
            }),
            MessLabel::AbandonedMidWay
        );
        // Old uncommitted work is "abandoned" even with a low score.
        let old = MessInputs {
            uncommitted_files: 1,
            oldest_uncommitted_days: Some(15.0),
            ..Default::default()
        };
        assert!(mess(&old, &w).score < 50.0);
        assert_eq!(label(old), MessLabel::AbandonedMidWay);

        let heavy = ScoreWeights {
            no_upstream: 200.0,
            ..ScoreWeights::default()
        };
        assert_eq!(
            mess(
                &MessInputs {
                    no_upstream: true,
                    ..Default::default()
                },
                &heavy
            )
            .score,
            100.0
        );
        let zero = ScoreWeights {
            operation_in_progress: 0.0,
            ..ScoreWeights::default()
        };
        assert_eq!(
            mess(
                &MessInputs {
                    operation_in_progress: true,
                    ..Default::default()
                },
                &zero
            )
            .label,
            MessLabel::Clean
        );
    }
}
