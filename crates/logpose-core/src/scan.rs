//! Scan orchestrator (SCAN-001): discovery → git → sessions → scores → snapshot rows. Zero tokens.
//!
//! Sub-repos are aggregated into their parent: their dirtiness, unpushed commits, stashes and
//! branches add to the parent's mess inputs, and their last commit counts toward staleness.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::discovery::{self, DiscoveredProject, ExcludeSet};
use crate::git::{self, DirtyStats, RepoSignals, UnmergedBranch};
use crate::mtime::{self, MtimeScan};
use crate::score::{self, Mess, MessInputs, Staleness};
use crate::sessions::codex::RateLimit;
use crate::sessions::merge::normalize_path;
use crate::sessions::{self, ProjectActivity, ScanStats, Session, claude, codex};
use crate::store::{Snapshot, Store};

/// Prompts kept per project for the card/detail view and the context pack.
pub const LAST_PROMPTS: usize = 5;
/// Commits inspected for newly added TODO/FIXME markers.
pub const TODO_COMMITS: usize = 10;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RepoReport {
    pub path: PathBuf,
    pub signals: Option<RepoSignals>,
    pub dirty: Option<DirtyStats>,
    pub unmerged_branches: Vec<UnmergedBranch>,
    pub todos_added: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectReport {
    pub project: DiscoveredProject,
    /// The project's own repo (`None` when it has no VCS).
    pub repo: Option<RepoReport>,
    pub sub_repos: Vec<RepoReport>,
    pub mtime: MtimeScan,
    pub activity: Option<ProjectActivity>,
    pub mess_inputs: Option<MessInputs>,
    pub staleness: Option<Staleness>,
    /// `None` for projects without VCS.
    pub mess: Option<Mess>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ScanReport {
    pub taken_at: DateTime<Utc>,
    pub projects: Vec<ProjectReport>,
    pub claude_stats: ScanStats,
    pub codex_stats: ScanStats,
    pub codex_rate_limit: Option<RateLimit>,
    pub duration: Duration,
}

/// Where to read agent logs from; `None` skips that agent.
#[derive(Debug, Clone, Default)]
pub struct LogSources {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
}

impl LogSources {
    pub fn defaults() -> Self {
        Self {
            claude: claude::default_base(),
            codex: codex::default_base(),
        }
    }
}

pub fn scan(config: &Config, logs: &LogSources, now: DateTime<Utc>) -> Result<ScanReport, globset::Error> {
    let start = Instant::now();
    let exclude = ExcludeSet::new(&config.exclude)?;

    // PERF-001: the agent logs are read while the projects found on disk are discovered and their
    // git and file-time work runs; only projects found through the logs alone wait for the logs.
    let ((claude_sessions, claude_stats), codex_scan, found, mut heavy) = std::thread::scope(|scope| {
        let claude = scope.spawn(|| {
            logs.claude
                .as_deref()
                .map(claude::read_sessions)
                .unwrap_or_default()
        });
        let codex = scope.spawn(|| {
            logs.codex
                .as_deref()
                .map(codex::read_sessions)
                .unwrap_or_default()
        });
        let found = discovery::apply_config(
            discovery::discover(&config.roots, config.scan_depth),
            &exclude,
            config,
        );
        let heavy: std::collections::HashMap<PathBuf, Heavy> = found
            .iter()
            .map(|p| p.path.clone())
            .zip(parallel_map(&found, |_, p| heavy_scan(p)))
            .collect();
        (
            claude.join().expect("claude log reader"),
            codex.join().expect("codex log reader"),
            found,
            heavy,
        )
    });
    let sessions: Vec<Session> = claude_sessions.into_iter().chain(codex_scan.sessions).collect();
    let cwds: Vec<PathBuf> = sessions.iter().map(|s| normalize_path(&s.cwd)).collect();
    let projects = discovery::merge_log_cwds(found, cwds.iter().cloned(), &exclude, config);
    let missing: Vec<&DiscoveredProject> = projects.iter().filter(|p| !heavy.contains_key(&p.path)).collect();
    for (p, h) in missing.iter().zip(parallel_map(&missing, |_, p| heavy_scan(p))) {
        heavy.insert(p.path.clone(), h);
    }

    let project_paths: Vec<PathBuf> = projects.iter().map(|p| normalize_path(&p.path)).collect();
    let mut per_project: Vec<Vec<&Session>> = vec![Vec::new(); projects.len()];
    for (session, cwd) in sessions.iter().zip(&cwds) {
        if let Some(i) = attribute(&project_paths, cwd) {
            per_project[i].push(session);
        }
    }

    let reports: Vec<ProjectReport> = projects
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let h = heavy.remove(&p.path).expect("every project was scanned");
            scan_project(p, h, &per_project[i], config, now)
        })
        .collect();
    Ok(ScanReport {
        taken_at: now,
        projects: reports,
        claude_stats,
        codex_stats: codex_scan.stats,
        codex_rate_limit: codex_scan.rate_limit,
        duration: start.elapsed(),
    })
}

/// Index of the most specific project (normalized path) containing the normalized `cwd`.
pub fn attribute(project_paths: &[PathBuf], cwd: &Path) -> Option<usize> {
    project_paths
        .iter()
        .enumerate()
        .filter(|(_, p)| cwd.starts_with(p))
        .max_by_key(|(_, p)| p.components().count())
        .map(|(i, _)| i)
}

/// `todos_added` depends only on the repo and its HEAD commit, so it is kept for the life of the
/// process (rescans in the app and in tray mode reuse it; PERF-001).
fn todos_cached(path: &Path, head: Option<&str>) -> usize {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<(PathBuf, String), usize>>> =
        std::sync::OnceLock::new();
    let Some(head) = head else {
        return 0;
    };
    let cache = CACHE.get_or_init(Default::default);
    let key = (path.to_owned(), head.to_owned());
    if let Some(n) = cache.lock().ok().and_then(|c| c.get(&key).copied()) {
        return n;
    }
    let n = git::todos_added(path, TODO_COMMITS).unwrap_or(0);
    if let Ok(mut c) = cache.lock() {
        c.insert(key, n);
    }
    n
}

fn scan_repo_report(path: &Path) -> RepoReport {
    let mut r = RepoReport {
        path: path.to_owned(),
        ..RepoReport::default()
    };
    match git::scan_repo_with_dirty(path) {
        Ok((signals, dirty)) => {
            if let Some(default) = &signals.default_branch {
                r.unmerged_branches = git::unmerged_branches(path, default).unwrap_or_default();
            }
            r.dirty = Some(dirty);
            r.todos_added = todos_cached(path, signals.head_id.as_deref());
            r.signals = Some(signals);
        }
        Err(e) => r.error = Some(e.to_string()),
    }
    r
}

/// The slow, session-independent part of a project scan: git and file times.
struct Heavy {
    repo: Option<RepoReport>,
    sub_repos: Vec<RepoReport>,
    mtime: MtimeScan,
}

fn heavy_scan(p: &DiscoveredProject) -> Heavy {
    Heavy {
        repo: p.markers.git.then(|| scan_repo_report(&p.path)),
        sub_repos: p.sub_repos.iter().map(|s| scan_repo_report(s)).collect(),
        mtime: mtime::scan(&p.path),
    }
}

fn scan_project(
    p: &DiscoveredProject,
    heavy: Heavy,
    sessions: &[&Session],
    config: &Config,
    now: DateTime<Utc>,
) -> ProjectReport {
    let Heavy {
        repo,
        sub_repos,
        mtime,
    } = heavy;
    let activity = sessions::summarize(sessions, now, LAST_PROMPTS);

    let all_repos: Vec<&RepoReport> = repo
        .iter()
        .chain(&sub_repos)
        .filter(|r| r.signals.is_some())
        .collect();
    let last_commit = all_repos
        .iter()
        .filter_map(|r| r.signals.as_ref()?.last_commit_at)
        .max();
    let staleness = score::staleness(
        last_commit,
        mtime.latest.as_ref().map(|l| l.modified_at),
        activity.as_ref().map(|a| a.last_session_at),
        now,
    );

    // A root without a repo is scored from its sub-repos alone (SCORE-004).
    let own = repo.as_ref().and_then(|r| r.signals.as_ref());
    let mess_inputs = (own.is_some() || !all_repos.is_empty()).then(|| {
        let mut i = MessInputs {
            // The parent's own upstream; without a root repo, any sub-repo lacking one.
            no_upstream: own.map_or_else(
                || {
                    all_repos
                        .iter()
                        .any(|r| r.signals.as_ref().is_some_and(|s| s.no_upstream()))
                },
                |s| s.no_upstream(),
            ),
            days_inactive: staleness.map(|s| s.days),
            ..MessInputs::default()
        };
        for r in &all_repos {
            let s = r.signals.as_ref().expect("filtered");
            i.operation_in_progress |= s.operation.is_some() || s.conflicted > 0;
            i.uncommitted_files += s.uncommitted_files();
            i.unpushed_commits += s.ahead;
            i.stash_count += s.stash_count;
            i.stale_branches += r.unmerged_branches.len();
            if let Some(t) = r.dirty.as_ref().and_then(|d| d.oldest_change_at) {
                let days = ((now - t).num_seconds() as f64 / 86_400.0).max(0.0);
                i.oldest_uncommitted_days =
                    Some(i.oldest_uncommitted_days.map_or(days, |d: f64| d.max(days)));
            }
        }
        i
    });
    let mess = mess_inputs
        .as_ref()
        .map(|i| score::mess(i, &config.score.weights));
    ProjectReport {
        project: p.clone(),
        repo,
        sub_repos,
        mtime,
        activity,
        mess_inputs,
        staleness,
        mess,
    }
}

pub(crate) fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(usize, &T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8)
        .min(items.len().max(1));
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(items.len()));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(i, item);
                    results.lock().expect("no poisoned workers").push((i, r));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("no poisoned workers");
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, r)| r).collect()
}

/// Compact per-launch signals stored in `snapshots.signals_json`.
#[derive(Debug, Serialize)]
struct SnapshotSignals<'a> {
    staleness_bucket: Option<score::StalenessBucket>,
    mess_label: Option<score::MessLabel>,
    mess_inputs: Option<&'a MessInputs>,
    agents: Vec<sessions::Agent>,
    sessions_30d: usize,
    has_vcs: bool,
}

/// Upsert every scanned project and write one snapshot row per project. Returns project ids in report order.
pub fn persist(store: &Store, report: &ScanReport) -> rusqlite::Result<Vec<i64>> {
    let mut ids = Vec::with_capacity(report.projects.len());
    for p in &report.projects {
        let id = store.upsert_project(&p.project.path, &p.project.title, report.taken_at)?;
        let signals = SnapshotSignals {
            staleness_bucket: p.staleness.map(|s| s.bucket),
            mess_label: p.mess.map(|m| m.label),
            mess_inputs: p.mess_inputs.as_ref(),
            agents: p.activity.as_ref().map(|a| a.agents.clone()).unwrap_or_default(),
            sessions_30d: p.activity.as_ref().map_or(0, |a| a.sessions_30d),
            has_vcs: p.mtime.has_vcs,
        };
        store.insert_snapshot(&Snapshot {
            project_id: id,
            taken_at: report.taken_at,
            staleness_days: p.staleness.map(|s| s.days),
            mess_score: p.mess.map(|m| m.score),
            signals_json: serde_json::to_string(&signals).unwrap_or_else(|_| "{}".into()),
        })?;
        ids.push(id);
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testutil::{git, repo_with_upstream, write};
    use crate::score::{MessLabel, StalenessBucket};

    /// A one-session Claude log whose cwd is `cwd`.
    fn claude_log(base: &Path, cwd: &Path, ts: &str) {
        let enc: String = cwd
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = base.join("projects").join(enc);
        std::fs::create_dir_all(&dir).unwrap();
        let rec = |t: &str, extra: &str| {
            format!(
                r#"{{"type":"{t}","uuid":"u-{t}","parentUuid":null,"isSidechain":false,"timestamp":"{ts}","sessionId":"s1","cwd":"{}","gitBranch":"main","version":"2.1.304","entrypoint":"cli","userType":"external",{extra}}}"#,
                cwd.display()
            )
        };
        let user = rec(
            "user",
            r#""message":{"role":"user","content":"finish the parser"}"#,
        );
        let asst = rec(
            "assistant",
            r#""requestId":"r1","message":{"id":"m1","model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"Parser half done."}],"usage":{"input_tokens":1,"output_tokens":2}}"#,
        );
        std::fs::write(dir.join("s1.jsonl"), format!("{user}\n{asst}\n")).unwrap();
    }

    #[test]
    fn end_to_end_over_temp_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        // Git project with upstream, a dirty file and a nested sub-repo with a stash.
        let w = repo_with_upstream(&root);
        write(&w, "f.txt", "dirty\n");
        let vendor = w.join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();
        git(&vendor, &["init", "-q", "-b", "main", "lib"]);
        let lib = vendor.join("lib");
        write(&lib, "l.txt", "1\n");
        git(&lib, &["add", "l.txt"]);
        git(&lib, &["commit", "-qm", "l1"]);
        write(&lib, "l.txt", "2\n");
        git(&lib, &["stash", "-q"]);
        // Non-git project known from CLAUDE.md, and one known only from logs.
        std::fs::create_dir_all(root.join("notes")).unwrap();
        write(&root.join("notes"), "CLAUDE.md", "# notes\n");
        std::fs::create_dir_all(root.join("scratch")).unwrap();
        write(&root.join("scratch"), "idea.txt", "x\n");
        let logs = dir.path().join("claude");
        claude_log(&logs, &root.join("scratch"), "2026-10-31T12:00:00.000Z");
        claude_log(&logs, &w.join("src"), "2026-10-30T12:00:00.000Z");

        let config = Config {
            roots: vec![root.clone()],
            ..Config::default()
        };
        let sources = LogSources {
            claude: Some(logs),
            codex: None,
        };
        let now = Utc::now();
        let report = scan(&config, &sources, now).unwrap();

        let titles: Vec<&str> = report.projects.iter().map(|p| p.project.title.as_str()).collect();
        assert_eq!(
            titles,
            ["notes", "scratch", "work"],
            "bare remote.git is not a project"
        );

        let work = report
            .projects
            .iter()
            .find(|p| p.project.title == "work")
            .unwrap();
        assert_eq!(work.sub_repos.len(), 1);
        let inputs = work.mess_inputs.as_ref().unwrap();
        // f.txt modified + vendor/ untracked in the parent (as `git status` reports); stash from the sub-repo.
        assert_eq!((inputs.uncommitted_files, inputs.stash_count), (2, 1));
        assert!(!inputs.no_upstream);
        assert_eq!(work.mess.unwrap().label, MessLabel::Clean);
        assert_eq!(work.staleness.unwrap().bucket, StalenessBucket::Fresh);
        let activity = work.activity.as_ref().unwrap();
        assert_eq!(
            activity.last_prompts[0].text, "finish the parser",
            "session in work/src belongs to work"
        );

        let scratch = report
            .projects
            .iter()
            .find(|p| p.project.title == "scratch")
            .unwrap();
        assert!(scratch.project.from_logs && scratch.mess.is_none() && scratch.activity.is_some());
        let notes = report
            .projects
            .iter()
            .find(|p| p.project.title == "notes")
            .unwrap();
        assert!(notes.repo.is_none() && !notes.mtime.has_vcs && notes.activity.is_none());

        let store = Store::open_in_memory().unwrap();
        let ids = persist(&store, &report).unwrap();
        assert_eq!(ids.len(), 3);
        persist(&store, &report).unwrap();
        let snaps = store.snapshots(ids[2]).unwrap();
        assert_eq!(snaps.len(), 2, "one snapshot per persisted scan");
        let json: serde_json::Value = serde_json::from_str(&snaps[0].signals_json).unwrap();
        assert_eq!(json["mess_label"], "clean");
        assert_eq!(store.projects().unwrap().len(), 3);
    }

    #[test]
    fn root_without_repo_is_scored_from_sub_repos() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let parent = root.join("mobile");
        std::fs::create_dir_all(&parent).unwrap();
        write(&parent, "CLAUDE.md", "# mobile\n");
        // One sub-repo with an upstream, one without upstream and with a modified file.
        repo_with_upstream(&parent);
        git(&parent, &["init", "-q", "-b", "main", "droid"]);
        let droid = parent.join("droid");
        write(&droid, "d.txt", "1\n");
        git(&droid, &["add", "d.txt"]);
        git(&droid, &["commit", "-qm", "d1"]);
        write(&droid, "d.txt", "2\n");

        let config = Config {
            roots: vec![root.clone()],
            ..Config::default()
        };
        let report = scan(&config, &LogSources::default(), Utc::now()).unwrap();
        assert_eq!(report.projects.len(), 1);
        let p = &report.projects[0];
        assert!(p.repo.is_none() && !p.mtime.has_vcs);
        assert_eq!(p.sub_repos.len(), 2);
        let inputs = p.mess_inputs.as_ref().expect("scored from sub-repos");
        assert_eq!(inputs.uncommitted_files, 1);
        assert!(inputs.no_upstream, "droid has no upstream");
        assert!(p.mess.is_some());

        // Without any repo there is still no score.
        std::fs::remove_dir_all(&droid).unwrap();
        std::fs::remove_dir_all(parent.join("work")).unwrap();
        let report = scan(&config, &LogSources::default(), Utc::now()).unwrap();
        assert!(report.projects[0].mess.is_none());
    }

    #[test]
    fn parallel_map_keeps_order() {
        let items: Vec<usize> = (0..100).collect();
        assert_eq!(parallel_map(&items, |i, x| i * 1000 + x * 2)[37], 37 * 1000 + 74);
        assert!(parallel_map(&Vec::<u8>::new(), |_, x| *x).is_empty());
    }
}
