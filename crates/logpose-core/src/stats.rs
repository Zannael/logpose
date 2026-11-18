//! Retroactive stats (BLUEPRINT §7.5). Computed from git history, so they exist from day one
//! without a daemon.
//!
//! STATS-001: commits per day across all projects (heatmap) and projects touched per week. Only
//! commits authored by the user count (their `user.email`), so forks of large upstream repos don't
//! drown the heatmap. Sub-repo commits count toward their parent project.
//!
//! STATS-002: agent usage split (Claude Code vs Codex) per project and per week, from session logs.
//!
//! STATS-003: from the store, projects per staleness bucket over time (one snapshot per project per
//! scan) and projects archived or moved to Maintenance ("finished", DEC-004) per month.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use serde::Serialize;

use crate::git;
use crate::scan::{self, ProjectReport};
use crate::score::StalenessBucket;
use crate::sessions::merge::normalize_path;
use crate::sessions::{Agent, Session};
use crate::store::{BoardColumn, ProjectRow, Snapshot};

/// One authored commit of a project (any of its repos).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCommit {
    pub project: PathBuf,
    pub id: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GitStats {
    /// Local calendar day → commits that day (each commit id counted once).
    pub commits_per_day: BTreeMap<NaiveDate, usize>,
    /// Monday of the local week → projects with at least one commit that week.
    pub projects_per_week: BTreeMap<NaiveDate, usize>,
    /// Lowercased author emails that were counted.
    pub emails: Vec<String>,
    /// Repositories that could not be read (`path: message`).
    pub errors: Vec<String>,
}

/// Monday of the week containing `day`.
pub fn week_start(day: NaiveDate) -> NaiveDate {
    day - chrono::Duration::days(day.weekday().num_days_from_monday().into())
}

/// Aggregate commits into the heatmap and weekly series, in the time zone `tz`.
pub fn aggregate<Tz: TimeZone>(commits: &[ProjectCommit], tz: &Tz) -> GitStats {
    let mut stats = GitStats::default();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut weeks: BTreeMap<NaiveDate, BTreeSet<&PathBuf>> = BTreeMap::new();
    for c in commits {
        let day = c.at.with_timezone(tz).date_naive();
        if seen.insert(&c.id) {
            *stats.commits_per_day.entry(day).or_default() += 1;
        }
        weeks.entry(week_start(day)).or_default().insert(&c.project);
    }
    stats.projects_per_week = weeks.into_iter().map(|(w, p)| (w, p.len())).collect();
    stats
}

/// Read authored commits since `since` from every project's own repo and sub-repos, then aggregate.
/// `emails` are matched case-insensitively; when empty, each repo's own `user.email` is used.
/// [`git_stats_for`] can count both.
pub fn git_stats<Tz: TimeZone>(
    projects: &[ProjectReport],
    emails: &[String],
    since: DateTime<Utc>,
    tz: &Tz,
) -> GitStats {
    let repos: Vec<(PathBuf, Vec<PathBuf>)> = projects
        .iter()
        .map(|p| {
            let repos = p.repo.iter().chain(&p.sub_repos).filter(|r| r.signals.is_some());
            (p.project.path.clone(), repos.map(|r| r.path.clone()).collect())
        })
        .collect();
    let who = if emails.is_empty() {
        Authors::RepoEmail(&[])
    } else {
        Authors::Only(emails)
    };
    git_stats_for(&repos, who, since, None, tz)
}

/// Whose commits count.
#[derive(Debug, Clone, Copy)]
pub enum Authors<'a> {
    /// Exactly these emails.
    Only(&'a [String]),
    /// Each repo's own `user.email` plus these (STATS-005: the user's older or other addresses).
    RepoEmail(&'a [String]),
}

/// [`git_stats`] over `(project path, repository paths)` pairs, optionally only commits before `until`
/// (a custom Stats interval, UI-002).
pub fn git_stats_for<Tz: TimeZone>(
    projects: &[(PathBuf, Vec<PathBuf>)],
    authors: Authors,
    since: DateTime<Utc>,
    until: Option<DateTime<Utc>>,
    tz: &Tz,
) -> GitStats {
    let mut commits = Vec::new();
    let mut errors = Vec::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for (project, repos) in projects {
        for repo in repos {
            let (own, extra) = match authors {
                Authors::Only(e) => (None, e),
                Authors::RepoEmail(e) => (git::user_email(repo), e),
            };
            let mut emails: Vec<String> = own.into_iter().collect();
            for e in extra.iter().map(|e| e.trim().to_lowercase()) {
                if !e.is_empty() && !emails.contains(&e) {
                    emails.push(e);
                }
            }
            if emails.is_empty() {
                continue;
            }
            used.extend(emails.iter().cloned());
            match git::authored_commits(repo, &emails, since) {
                Ok(list) => commits.extend(list.into_iter().map(|(id, at)| ProjectCommit {
                    project: project.clone(),
                    id,
                    at,
                })),
                Err(e) => errors.push(format!("{}: {}", e.path, e.message)),
            }
        }
    }
    if let Some(until) = until {
        commits.retain(|c| c.at < until);
    }
    let mut stats = aggregate(&commits, tz);
    stats.emails = used.into_iter().collect();
    stats.errors = errors;
    stats
}

/// Usage of one agent: sessions, real prompts and tokens (all buckets; Claude and Codex count
/// tokens differently, so compare them per agent, not across agents).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AgentUsage {
    pub sessions: usize,
    pub prompts: usize,
    pub tokens: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AgentSplit {
    pub claude_code: AgentUsage,
    pub codex: AgentUsage,
}

impl AgentSplit {
    fn add(&mut self, s: &Session) {
        let u = match s.agent {
            Agent::ClaudeCode => &mut self.claude_code,
            Agent::Codex => &mut self.codex,
        };
        u.sessions += 1;
        u.prompts += s.prompts.len();
        u.tokens += s.tokens.total();
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionStats {
    /// Per project path (only projects with sessions), sorted by path.
    pub per_project: BTreeMap<PathBuf, AgentSplit>,
    /// Monday of the local week (by session start) → split over **all** sessions.
    pub per_week: BTreeMap<NaiveDate, AgentSplit>,
    /// Sessions whose cwd is in no project (e.g. outside every root, DEC-003).
    pub unattributed: AgentSplit,
}

/// Split `sessions` by agent per project (most specific project containing the cwd, as in the scan)
/// and per week of their start time in `tz`.
pub fn session_stats<Tz: TimeZone>(sessions: &[Session], project_paths: &[PathBuf], tz: &Tz) -> SessionStats {
    let normalized: Vec<PathBuf> = project_paths.iter().map(|p| normalize_path(p)).collect();
    let mut stats = SessionStats::default();
    for s in sessions {
        match scan::attribute(&normalized, &normalize_path(&s.cwd)) {
            Some(i) => stats
                .per_project
                .entry(project_paths[i].clone())
                .or_default()
                .add(s),
            None => stats.unattributed.add(s),
        }
        let week = week_start(s.started_at.with_timezone(tz).date_naive());
        stats.per_week.entry(week).or_default().add(s);
    }
    stats
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct BucketCounts {
    pub fresh: usize,
    pub cooling: usize,
    pub stale: usize,
    pub dormant: usize,
    /// No known activity (`staleness_days` NULL).
    pub unknown: usize,
}

/// Local day → projects per staleness bucket, using each project's **last** snapshot of that day.
/// Days without a scan have no entry (there is no daemon).
pub fn bucket_series<Tz: TimeZone>(snapshots: &[Snapshot], tz: &Tz) -> BTreeMap<NaiveDate, BucketCounts> {
    let mut last: BTreeMap<(NaiveDate, i64), (DateTime<Utc>, Option<f64>)> = BTreeMap::new();
    for s in snapshots {
        let key = (s.taken_at.with_timezone(tz).date_naive(), s.project_id);
        let entry = last.entry(key).or_insert((s.taken_at, s.staleness_days));
        if s.taken_at >= entry.0 {
            *entry = (s.taken_at, s.staleness_days);
        }
    }
    let mut series: BTreeMap<NaiveDate, BucketCounts> = BTreeMap::new();
    for ((day, _), (_, days)) in last {
        let c = series.entry(day).or_default();
        match days.map(StalenessBucket::from_days) {
            Some(StalenessBucket::Fresh) => c.fresh += 1,
            Some(StalenessBucket::Cooling) => c.cooling += 1,
            Some(StalenessBucket::Stale) => c.stale += 1,
            Some(StalenessBucket::Dormant) => c.dormant += 1,
            None => c.unknown += 1,
        }
    }
    series
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MonthMoves {
    pub archived: usize,
    /// Moved to Maintenance, the "finished" column (DEC-004).
    pub maintenance: usize,
}

/// First day of the local month → projects that entered Archived / Maintenance that month. Based on
/// the current board: a project that later left the column no longer counts.
pub fn moves_per_month<Tz: TimeZone>(rows: &[ProjectRow], tz: &Tz) -> BTreeMap<NaiveDate, MonthMoves> {
    let month = |t: DateTime<Utc>| {
        t.with_timezone(tz)
            .date_naive()
            .with_day(1)
            .expect("day 1 exists")
    };
    let mut out: BTreeMap<NaiveDate, MonthMoves> = BTreeMap::new();
    for r in rows {
        match (r.column, r.archived_at, r.maintenance_at) {
            (BoardColumn::Archived, Some(t), _) => out.entry(month(t)).or_default().archived += 1,
            (BoardColumn::Maintenance, _, Some(t)) => out.entry(month(t)).or_default().maintenance += 1,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::git::testutil::{git, write};
    use crate::scan::{self, LogSources};
    use chrono::FixedOffset;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn aggregates_days_weeks_and_dedups_ids() {
        let c = |p: &str, id: &str, at: &str| ProjectCommit {
            project: PathBuf::from(p),
            id: id.into(),
            at: t(at),
        };
        let commits = [
            c("/a", "1", "2026-10-26T09:00:00Z"), // Monday
            c("/a", "2", "2026-10-26T18:00:00Z"),
            c("/b", "3", "2026-11-01T23:30:00Z"), // Sunday UTC, Monday 00:30 at +01:00
            c("/b", "3", "2026-11-01T23:30:00Z"), // same commit seen in two repos
            c("/c", "4", "2026-11-03T10:00:00Z"),
        ];
        let utc = aggregate(&commits, &Utc);
        assert_eq!(
            utc.commits_per_day,
            BTreeMap::from([(d("2026-10-26"), 2), (d("2026-11-01"), 1), (d("2026-11-03"), 1)])
        );
        assert_eq!(
            utc.projects_per_week,
            BTreeMap::from([(d("2026-10-26"), 2), (d("2026-11-02"), 1)])
        );
        // In +01:00 the late Sunday commit falls on Monday and moves to the next week.
        let cet = aggregate(&commits, &FixedOffset::east_opt(3600).unwrap());
        assert_eq!(cet.commits_per_day[&d("2026-11-02")], 1);
        assert_eq!(
            cet.projects_per_week,
            BTreeMap::from([(d("2026-10-26"), 1), (d("2026-11-02"), 2)])
        );
        assert_eq!(week_start(d("2026-11-08")), d("2026-11-02"));
    }

    fn commit(dir: &std::path::Path, file: &str, date: &str, author: &str) {
        write(dir, file, date);
        git(dir, &["add", file]);
        git(
            dir,
            &[
                "commit",
                "-qm",
                file,
                &format!("--date={date}"),
                &format!("--author={author}"),
            ],
        );
    }

    #[test]
    fn extra_emails_count_with_the_repo_email() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        git(&a, &["init", "-q", "-b", "main"]);
        git(&a, &["config", "user.email", "Now@example.com"]);
        commit(&a, "1.txt", "2026-10-27T10:00:00Z", "Me <now@example.com>");
        commit(&a, "2.txt", "2026-10-28T10:00:00Z", "Me <old@uni.example>");
        commit(&a, "3.txt", "2026-10-29T10:00:00Z", "Other <other@example.com>");
        let pairs = vec![(a.clone(), vec![a.clone()])];
        let since = t("2026-10-01T00:00:00Z");
        let extra = vec![" OLD@uni.example ".to_string(), "now@example.com".to_string()];
        let s = git_stats_for(&pairs, Authors::RepoEmail(&extra), since, None, &Utc);
        assert_eq!(
            s.commits_per_day,
            BTreeMap::from([(d("2026-10-27"), 1), (d("2026-10-28"), 1)])
        );
        assert_eq!(s.emails, ["now@example.com", "old@uni.example"]);
        // Without extras only the repo's own email counts.
        let s = git_stats_for(&pairs, Authors::RepoEmail(&[]), since, None, &Utc);
        assert_eq!(s.commits_per_day, BTreeMap::from([(d("2026-10-27"), 1)]));
    }

    #[test]
    fn fixture_repos_give_expected_aggregates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let me = "Me <ME@example.com>";
        // Project a: two of my commits on one day, one by someone else, one before the cutoff.
        let a = root.join("a");
        std::fs::create_dir_all(&a).unwrap();
        git(&a, &["init", "-q", "-b", "main"]);
        commit(&a, "old.txt", "2026-01-05T10:00:00Z", me);
        commit(&a, "1.txt", "2026-10-27T10:00:00Z", me);
        commit(&a, "2.txt", "2026-10-27T15:00:00Z", me);
        commit(&a, "3.txt", "2026-10-28T10:00:00Z", "Other <other@example.com>");
        // Project b: one commit on a side branch, and one in a sub-repo the following week.
        let b = root.join("b");
        std::fs::create_dir_all(&b).unwrap();
        git(&b, &["init", "-q", "-b", "main"]);
        commit(&b, "1.txt", "2026-10-20T10:00:00Z", me);
        git(&b, &["checkout", "-qb", "feature"]);
        commit(&b, "2.txt", "2026-10-29T10:00:00Z", me);
        git(&b, &["checkout", "-q", "main"]);
        git(&b, &["init", "-q", "-b", "main", "lib"]);
        commit(&b.join("lib"), "l.txt", "2026-11-03T10:00:00Z", me);

        let config = Config {
            roots: vec![root.clone()],
            ..Config::default()
        };
        let report = scan::scan(&config, &LogSources::default(), Utc::now()).unwrap();
        let emails = vec!["me@example.com".to_string()];
        let s = git_stats(&report.projects, &emails, t("2026-10-01T00:00:00Z"), &Utc);
        assert_eq!(
            s.commits_per_day,
            BTreeMap::from([
                (d("2026-10-20"), 1),
                (d("2026-10-27"), 2),
                (d("2026-10-29"), 1),
                (d("2026-11-03"), 1),
            ])
        );
        assert_eq!(
            s.projects_per_week,
            BTreeMap::from([(d("2026-10-19"), 1), (d("2026-10-26"), 2), (d("2026-11-02"), 1)])
        );
        assert!(s.errors.is_empty());
        assert_eq!(s.emails, emails);

        let pairs: Vec<(PathBuf, Vec<PathBuf>)> = report
            .projects
            .iter()
            .map(|p| {
                let repos = p.repo.iter().chain(&p.sub_repos).map(|r| r.path.clone());
                (p.project.path.clone(), repos.collect())
            })
            .collect();
        let s = git_stats_for(
            &pairs,
            Authors::Only(&emails),
            t("2026-10-21T00:00:00Z"),
            Some(t("2026-10-29T00:00:00Z")),
            &Utc,
        );
        assert_eq!(
            s.commits_per_day,
            BTreeMap::from([(d("2026-10-27"), 2)]),
            "only commits in [since, until)"
        );
        assert_eq!(s.projects_per_week, BTreeMap::from([(d("2026-10-26"), 1)]));
    }

    fn fixture_sessions() -> Vec<Session> {
        let base = crate::sessions::fixtures();
        let (mut sessions, _) = crate::sessions::claude::read_sessions(&base.join("claude"));
        sessions.extend(crate::sessions::codex::read_sessions(&base.join("codex")).sessions);
        sessions
    }

    #[test]
    fn session_split_over_fixture_logs() {
        let sessions = fixture_sessions();
        let my = PathBuf::from("/home/user/dev/My Project");
        // Only "My Project" is a project: the two sessions in /home/user/dev/other are unattributed.
        let s = session_stats(&sessions, std::slice::from_ref(&my), &Utc);
        assert_eq!(s.per_project.len(), 1);
        let split = s.per_project[&my];
        assert_eq!((split.claude_code.sessions, split.claude_code.prompts), (2, 4));
        assert_eq!((split.codex.sessions, split.codex.prompts), (1, 2));
        assert!(
            split.claude_code.tokens >= 11_234,
            "session 1111 alone has 11 234 tokens"
        );
        assert_eq!(
            (s.unattributed.claude_code.sessions, s.unattributed.codex.sessions),
            (1, 1)
        );

        let week = |w: &str| s.per_week[&d(w)];
        assert_eq!(s.per_week.len(), 4);
        assert_eq!(
            (
                week("2026-10-26").claude_code.sessions,
                week("2026-10-26").codex.sessions
            ),
            (1, 1)
        );
        assert_eq!(
            (
                week("2026-10-19").claude_code.sessions,
                week("2026-10-19").codex.sessions
            ),
            (1, 0)
        );
        assert_eq!(
            (
                week("2026-09-07").claude_code.sessions,
                week("2026-09-07").codex.sessions
            ),
            (0, 1)
        );
        assert_eq!(week("2026-08-31").claude_code.sessions, 1);

        // A nested project takes its sessions from the parent (most specific wins).
        let nested = PathBuf::from("/home/user/dev");
        let s = session_stats(&sessions, &[nested.clone(), my.clone()], &Utc);
        assert_eq!(s.per_project[&my].claude_code.sessions, 2);
        assert_eq!(
            s.per_project[&nested].claude_code.sessions, 1,
            "the `other` session"
        );
        assert_eq!(s.unattributed, AgentSplit::default());
    }

    #[test]
    fn bucket_series_over_seeded_snapshots() {
        use crate::store::Store;
        let store = Store::open_in_memory().unwrap();
        let [a, b, c] = ["a", "b", "c"].map(|n| {
            store
                .upsert_project(
                    std::path::Path::new(&format!("/p/{n}")),
                    n,
                    t("2026-10-01T00:00:00Z"),
                )
                .unwrap()
        });
        let snap = |id: i64, at: &str, days: Option<f64>| {
            store
                .insert_snapshot(&Snapshot {
                    project_id: id,
                    taken_at: t(at),
                    staleness_days: days,
                    mess_score: None,
                    signals_json: "{}".into(),
                })
                .unwrap()
        };
        // Day 1: two scans; the later one wins for `a` (fresh → cooling).
        snap(a, "2026-11-01T08:00:00Z", Some(1.0));
        snap(b, "2026-11-01T08:00:00Z", Some(20.0));
        snap(c, "2026-11-01T08:00:00Z", None);
        snap(a, "2026-11-01T20:00:00Z", Some(3.5));
        // Day 3 (no scan on day 2).
        snap(a, "2026-11-03T09:00:00Z", Some(0.2));
        snap(b, "2026-11-03T09:00:00Z", Some(61.0));
        // Before the window.
        snap(c, "2026-09-01T09:00:00Z", Some(1.0));

        let rows = store.snapshots_since(t("2026-10-01T00:00:00Z")).unwrap();
        assert_eq!(rows.len(), 6);
        let series = bucket_series(&rows, &Utc);
        assert_eq!(
            series,
            BTreeMap::from([
                (
                    d("2026-11-01"),
                    BucketCounts {
                        cooling: 1,
                        stale: 1,
                        unknown: 1,
                        ..Default::default()
                    }
                ),
                (
                    d("2026-11-03"),
                    BucketCounts {
                        fresh: 1,
                        dormant: 1,
                        ..Default::default()
                    }
                ),
            ])
        );
    }

    #[test]
    fn archived_and_finished_per_month() {
        use crate::store::Store;
        let mut store = Store::open_in_memory().unwrap();
        let ids: Vec<i64> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| {
                store
                    .upsert_project(
                        std::path::Path::new(&format!("/p/{n}")),
                        n,
                        t("2026-09-01T00:00:00Z"),
                    )
                    .unwrap()
            })
            .collect();
        store
            .move_before(ids[0], BoardColumn::Archived, None, t("2026-09-15T10:00:00Z"))
            .unwrap();
        store
            .move_before(ids[1], BoardColumn::Maintenance, None, t("2026-10-02T10:00:00Z"))
            .unwrap();
        store
            .move_before(ids[2], BoardColumn::Archived, None, t("2026-10-31T23:30:00Z"))
            .unwrap();
        // Archived, then brought back: no longer counts.
        store
            .move_before(ids[3], BoardColumn::Archived, None, t("2026-10-05T10:00:00Z"))
            .unwrap();
        store
            .move_before(ids[3], BoardColumn::Now, None, t("2026-10-06T10:00:00Z"))
            .unwrap();

        let rows = store.projects().unwrap();
        let m = |a, f| MonthMoves {
            archived: a,
            maintenance: f,
        };
        assert_eq!(
            moves_per_month(&rows, &Utc),
            BTreeMap::from([(d("2026-09-01"), m(1, 0)), (d("2026-10-01"), m(1, 1))])
        );
        // At +01:00 the late 31 October archive falls in November.
        assert_eq!(
            moves_per_month(&rows, &FixedOffset::east_opt(3600).unwrap()),
            BTreeMap::from([
                (d("2026-09-01"), m(1, 0)),
                (d("2026-10-01"), m(0, 1)),
                (d("2026-11-01"), m(1, 0))
            ])
        );
    }
}
