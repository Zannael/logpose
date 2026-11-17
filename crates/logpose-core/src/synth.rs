//! Synthesis orchestration (SYN-005): cache key, cache lookups, bounded concurrency, streamed results.
//!
//! The agent is only called for projects whose cache key changed since the last synthesis with that
//! agent. The store is used from the calling thread only; runner calls happen on worker threads and
//! results are delivered through `on_result` as each one completes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::next_md;
use crate::packer::{self, PackOptions};
use crate::runner::{INSTRUCTIONS, RunError, RunOutput, Runner};
use crate::scan::ProjectReport;
use crate::sessions::TokenUsage;
use crate::store::{CachedSynthesis, Store};
use crate::synthesis::{JSON_SCHEMA, Synthesis};

/// Everything that can change the synthesis, hashed. Bumps whenever instructions or schema change.
pub fn cache_key(r: &ProjectReport, opts: &PackOptions) -> String {
    let mut h = Sha256::new();
    let mut feed = |label: &str, value: &str| {
        h.update(label.as_bytes());
        h.update([0]);
        h.update(value.as_bytes());
        h.update([0]);
    };
    feed("v", "1");
    feed("instructions", INSTRUCTIONS);
    feed("schema", JSON_SCHEMA);
    for repo in r.repo.iter().chain(&r.sub_repos) {
        feed("repo", &repo.path.to_string_lossy());
        if let Some(s) = &repo.signals {
            feed("head", s.head_id.as_deref().unwrap_or("none"));
            feed("branch", s.branch.as_deref().unwrap_or("detached"));
            feed(
                "dirty",
                &format!("{}/{}/{}/{}", s.staged, s.modified, s.untracked, s.conflicted),
            );
            feed("op", &format!("{:?}", s.operation));
            feed("stash", &s.stash_count.to_string());
        }
        if let Some(d) = &repo.dirty {
            feed("lines", &format!("{}+{}-", d.lines_added, d.lines_removed));
        }
    }
    // Last file change catches edits that keep counts equal; covers non-git projects too.
    feed(
        "mtime",
        &r.mtime
            .latest
            .as_ref()
            .map(|l| l.modified_at.to_rfc3339())
            .unwrap_or_default(),
    );
    if let Some(a) = &r.activity {
        // Stand-in for "last session id": changes with every new or continued session.
        feed(
            "session",
            &format!("{}#{}", a.last_session_at.to_rfc3339(), a.sessions_total),
        );
    }
    if let Ok(next) = std::fs::read(r.project.path.join(crate::next_md::FILE_NAME)) {
        feed("next_md", &hex(&Sha256::digest(&next)));
    }
    feed("note", opts.parking_note.as_deref().unwrap_or(""));
    feed("conversation", &opts.include_conversation.to_string());
    feed("budget", &opts.budget_tokens.to_string());
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Cache,
    Agent,
    /// A current `NEXT.md` (SYN-006); no agent call and nothing cached.
    NextMd,
}

#[derive(Debug)]
pub struct SynthesisEvent {
    /// Index into the `projects` slice passed to [`synthesize`].
    pub index: usize,
    pub project_id: i64,
    pub result: Result<(Synthesis, Source), RunError>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SynthesisSummary {
    /// Skipped because `NEXT.md` is newer than the latest activity.
    pub next_md: usize,
    pub cached: usize,
    pub ran: usize,
    pub failed: usize,
    /// Batch calls made (SYN-012); each covers several projects.
    pub batches: usize,
    /// Single calls made because a batch call failed or one of its items failed validation.
    pub fallbacks: usize,
}

/// How to group cache misses into batch calls (SYN-012).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batching {
    /// Projects per call; 1 = no batching.
    pub max_projects: usize,
    /// Upper bound on the summed estimated pack tokens of one batch (a single oversized pack still
    /// gets its own call).
    pub max_tokens: usize,
}

impl Batching {
    pub const OFF: Batching = Batching {
        max_projects: 1,
        max_tokens: usize::MAX,
    };
    /// From the 2026-11-03 measurement (batch of 4 vs single): per project, Codex 16.4k → 4.5k
    /// tokens (its ~16k base instructions are shared) and Claude 3.1k → 1.4–1.6k. Six projects keep
    /// answers short and a failed batch cheap to retry.
    pub const RECOMMENDED: Batching = Batching {
        max_projects: 6,
        max_tokens: 12_000,
    };
}

/// Group `sizes` (estimated tokens, in order) greedily into batches within `b`'s limits.
pub fn plan_batches(sizes: &[usize], b: Batching) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut used = 0;
    for (i, &size) in sizes.iter().enumerate() {
        match out.last_mut() {
            Some(cur) if cur.len() < b.max_projects.max(1) && used + size <= b.max_tokens => {
                cur.push(i);
                used += size;
            }
            _ => {
                out.push(vec![i]);
                used = size;
            }
        }
    }
    out
}

/// Per-project inputs, resolved by the caller (parking note, privacy opt-out) from the store.
pub struct Job<'a> {
    pub report: &'a ProjectReport,
    pub project_id: i64,
    pub options: PackOptions,
    /// Call the agent even when the cache or a current `NEXT.md` has an answer (a user "Refresh").
    pub force: bool,
}

/// What the UI can show without calling an agent: a current `NEXT.md`, the cache entry for the
/// current key, or else the latest cached synthesis marked `outdated` (SYN-010).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Known {
    pub synthesis: Synthesis,
    pub source: Source,
    /// The project changed since this synthesis was made.
    pub outdated: bool,
    pub agent: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

/// Zero-token lookup for one job.
pub fn known(store: &Store, job: &Job<'_>) -> Option<Known> {
    let last_activity = job.report.staleness.map(|s| s.last_activity);
    if let Some(md) = next_md::read_current(&job.report.project.path, last_activity) {
        return Some(Known {
            synthesis: md.to_synthesis(),
            source: Source::NextMd,
            outdated: false,
            agent: md.header.map(|h| h.by),
            created_at: None,
        });
    }
    let key = cache_key(job.report, &job.options);
    let exact = store.cached_synthesis(job.project_id, &key).ok().flatten();
    let (cached, outdated) = match exact {
        Some(c) => (c, false),
        None => (store.latest_synthesis(job.project_id).ok().flatten()?, true),
    };
    Some(Known {
        synthesis: serde_json::from_str(&cached.result_json).ok()?,
        source: Source::Cache,
        outdated,
        agent: Some(cached.agent),
        created_at: Some(cached.created_at),
    })
}

/// Synthesize `jobs` with `runner`, at most `concurrency` agent calls at a time, one call per project.
pub fn synthesize(
    store: &Store,
    jobs: &[Job<'_>],
    runner: &dyn Runner,
    concurrency: usize,
    now: DateTime<Utc>,
    on_result: impl FnMut(SynthesisEvent),
) -> SynthesisSummary {
    synthesize_batched(store, jobs, runner, concurrency, Batching::OFF, now, on_result)
}

/// One finished project from a worker: the todo index, and the synthesis with its share of tokens.
type Done = (usize, Result<RunOutput, RunError>);

/// Like [`synthesize`], grouping cache misses into batch calls per `batching` (SYN-012). A batch
/// call's tokens are split evenly over its projects. Items that fail validation, and every item of
/// a batch call that fails, are retried with single calls.
pub fn synthesize_batched(
    store: &Store,
    jobs: &[Job<'_>],
    runner: &dyn Runner,
    concurrency: usize,
    batching: Batching,
    now: DateTime<Utc>,
    mut on_result: impl FnMut(SynthesisEvent),
) -> SynthesisSummary {
    let mut summary = SynthesisSummary::default();
    let mut todo: Vec<(usize, String, String)> = Vec::new(); // (index, key, pack text)
    let mut sizes: Vec<usize> = Vec::new();
    for (index, job) in jobs.iter().enumerate() {
        let last_activity = job.report.staleness.map(|s| s.last_activity);
        let current_md = (!job.force)
            .then(|| next_md::read_current(&job.report.project.path, last_activity))
            .flatten();
        if let Some(md) = current_md {
            summary.next_md += 1;
            on_result(SynthesisEvent {
                index,
                project_id: job.project_id,
                result: Ok((md.to_synthesis(), Source::NextMd)),
            });
            continue;
        }
        let key = cache_key(job.report, &job.options);
        let cached = (!job.force)
            .then(|| store.cached_synthesis(job.project_id, &key).ok().flatten())
            .flatten();
        if let Some(s) = cached.and_then(|c| serde_json::from_str::<Synthesis>(&c.result_json).ok()) {
            summary.cached += 1;
            on_result(SynthesisEvent {
                index,
                project_id: job.project_id,
                result: Ok((s, Source::Cache)),
            });
        } else {
            let pack = packer::pack(job.report, &job.options, now);
            sizes.push(pack.estimated_tokens);
            todo.push((index, key, pack.text));
        }
    }
    if todo.is_empty() {
        return summary;
    }

    let units = plan_batches(&sizes, batching);
    let batches = AtomicUsize::new(0);
    let fallbacks = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<Done>();
    let next = AtomicUsize::new(0);
    let workers = concurrency.max(1).min(units.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (todo, units, next, batches, fallbacks) = (&todo, &units, &next, &batches, &fallbacks);
            scope.spawn(move || {
                loop {
                    let u = next.fetch_add(1, Ordering::Relaxed);
                    let Some(unit) = units.get(u) else {
                        break;
                    };
                    for done in run_unit(runner, todo, unit, batches, fallbacks) {
                        if tx.send(done).is_err() {
                            return;
                        }
                    }
                }
            });
        }
        drop(tx);
        // Results are handled here, on the caller's thread, as they arrive.
        for (t, result) in rx {
            let (index, key, _) = &todo[t];
            let job = &jobs[*index];
            let result = match result {
                Ok(out) => {
                    summary.ran += 1;
                    let _ = store.put_synthesis(&CachedSynthesis {
                        project_id: job.project_id,
                        cache_key: key.clone(),
                        agent: runner.name().to_string(),
                        model: out.model.clone(),
                        result_json: serde_json::to_string(&out.synthesis).unwrap_or_default(),
                        tokens_in: out.tokens.input + out.tokens.cache_read + out.tokens.cache_creation,
                        tokens_out: out.tokens.output,
                        created_at: now,
                    });
                    Ok((out.synthesis, Source::Agent))
                }
                Err(e) => {
                    summary.failed += 1;
                    Err(e)
                }
            };
            on_result(SynthesisEvent {
                index: *index,
                project_id: job.project_id,
                result,
            });
        }
    });
    summary.batches = batches.into_inner();
    summary.fallbacks = fallbacks.into_inner();
    summary
}

/// Run one planned unit: a single call, or a batch call with single-call fallbacks.
fn run_unit(
    runner: &dyn Runner,
    todo: &[(usize, String, String)],
    unit: &[usize],
    batches: &AtomicUsize,
    fallbacks: &AtomicUsize,
) -> Vec<Done> {
    let single = |t: usize| (t, runner.run(&todo[t].2));
    if let [t] = unit {
        return vec![single(*t)];
    }
    let ids: Vec<String> = unit.iter().map(|t| format!("p{t}")).collect();
    let packs: Vec<(&str, &str)> = unit
        .iter()
        .zip(&ids)
        .map(|(t, id)| (id.as_str(), todo[*t].2.as_str()))
        .collect();
    batches.fetch_add(1, Ordering::Relaxed);
    let Ok(batch) = runner.run_batch(&packs) else {
        fallbacks.fetch_add(unit.len(), Ordering::Relaxed);
        return unit.iter().map(|t| single(*t)).collect();
    };
    let share = split_tokens(&batch.tokens, unit.len());
    unit.iter()
        .zip(batch.results)
        .map(|(t, r)| match r {
            Ok(synthesis) => (
                *t,
                Ok(RunOutput {
                    synthesis,
                    tokens: share,
                    model: batch.model.clone(),
                    duration: batch.duration,
                }),
            ),
            Err(_) => {
                fallbacks.fetch_add(1, Ordering::Relaxed);
                single(*t)
            }
        })
        .collect()
}

/// Even share of a batch call's usage per project (rounded down).
fn split_tokens(t: &TokenUsage, n: usize) -> TokenUsage {
    let n = n.max(1) as u64;
    TokenUsage {
        input: t.input / n,
        output: t.output / n,
        cache_read: t.cache_read / n,
        cache_creation: t.cache_creation / n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::git::testutil::{git, write};
    use crate::runner::RunOutput;
    use crate::scan::{self, LogSources};
    use crate::sessions::TokenUsage;
    use crate::synthesis::Confidence;
    use std::sync::Mutex;
    use std::time::Duration;

    struct FakeRunner {
        calls: AtomicUsize,
        running: AtomicUsize,
        max_running: AtomicUsize,
        fail_on: Option<String>,
        packs: Mutex<Vec<String>>,
    }

    impl FakeRunner {
        fn new(fail_on: Option<&str>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                running: AtomicUsize::new(0),
                max_running: AtomicUsize::new(0),
                fail_on: fail_on.map(str::to_string),
                packs: Mutex::new(Vec::new()),
            }
        }
    }

    impl Runner for FakeRunner {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn run(&self, pack: &str) -> Result<RunOutput, RunError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_running.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            self.running.fetch_sub(1, Ordering::SeqCst);
            self.packs.lock().unwrap().push(pack.to_string());
            if self.fail_on.as_deref().is_some_and(|f| pack.contains(f)) {
                return Err(RunError::Agent("boom".into()));
            }
            let name = pack.lines().next().unwrap_or_default().to_string();
            Ok(RunOutput {
                synthesis: Synthesis {
                    description: name,
                    state: "ok".into(),
                    last_goal: String::new(),
                    next_steps: vec!["step".into()],
                    blockers: vec![],
                    confidence: Confidence::Low,
                },
                tokens: TokenUsage {
                    input: 10,
                    output: 5,
                    ..Default::default()
                },
                model: Some("fake-1".into()),
                duration: Duration::ZERO,
            })
        }
    }

    /// Batches like the real runners, but the item whose pack contains `bad_item` comes back invalid.
    struct BatchFake {
        single: FakeRunner,
        batch_calls: AtomicUsize,
        batch_sizes: Mutex<Vec<usize>>,
        bad_item: Option<String>,
    }

    impl Runner for BatchFake {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn run(&self, pack: &str) -> Result<RunOutput, RunError> {
            self.single.run(pack)
        }

        fn run_batch(&self, packs: &[(&str, &str)]) -> Result<crate::runner::BatchOutput, RunError> {
            self.batch_calls.fetch_add(1, Ordering::SeqCst);
            self.batch_sizes.lock().unwrap().push(packs.len());
            let items: Vec<String> = packs
                .iter()
                .map(|(id, pack)| {
                    let desc = if self.bad_item.as_deref().is_some_and(|b| pack.contains(b)) {
                        String::new()
                    } else {
                        format!("batch {}", pack.lines().next().unwrap_or_default())
                    };
                    serde_json::json!({"id": id, "description": desc, "state": "s", "last_goal": "",
                        "next_steps": ["n"], "blockers": [], "confidence": "low"})
                    .to_string()
                })
                .collect();
            let raw = format!(r#"{{"projects":[{}]}}"#, items.join(","));
            let ids: Vec<&str> = packs.iter().map(|(id, _)| *id).collect();
            Ok(crate::runner::BatchOutput {
                results: crate::synthesis::parse_batch(&raw, &ids),
                tokens: TokenUsage {
                    input: 90,
                    output: 30,
                    ..Default::default()
                },
                model: Some("fake-batch".into()),
                duration: Duration::ZERO,
            })
        }
    }

    #[test]
    fn plan_respects_count_and_token_limits() {
        let b = Batching {
            max_projects: 3,
            max_tokens: 1000,
        };
        assert_eq!(
            plan_batches(&[100, 200, 300, 400, 50], b),
            [vec![0, 1, 2], vec![3, 4]]
        );
        assert_eq!(
            plan_batches(&[900, 200, 1500, 10], b),
            [vec![0], vec![1], vec![2], vec![3]]
        );
        assert_eq!(
            plan_batches(&[1, 1, 1], Batching::OFF),
            [vec![0], vec![1], vec![2]]
        );
        assert!(plan_batches(&[], b).is_empty());
    }

    fn run_batched(
        store: &Store,
        reports: &[ProjectReport],
        runner: &dyn Runner,
        b: Batching,
    ) -> (SynthesisSummary, Vec<SynthesisEvent>) {
        let jobs: Vec<Job> = reports
            .iter()
            .map(|r| Job {
                report: r,
                project_id: store
                    .upsert_project(&r.project.path, &r.project.title, Utc::now())
                    .unwrap(),
                options: PackOptions::default(),
                force: false,
            })
            .collect();
        let mut events = Vec::new();
        let summary = synthesize_batched(store, &jobs, runner, 2, b, Utc::now(), |e| events.push(e));
        (summary, events)
    }

    #[test]
    fn batches_with_fallback_for_invalid_items() {
        let (_dir, reports) = setup(5);
        let store = Store::open_in_memory().unwrap();
        let runner = BatchFake {
            single: FakeRunner::new(None),
            batch_calls: AtomicUsize::new(0),
            batch_sizes: Mutex::new(Vec::new()),
            bad_item: Some("Project: p1 ".into()),
        };
        let b = Batching {
            max_projects: 3,
            max_tokens: 100_000,
        };
        let (summary, events) = run_batched(&store, &reports, &runner, b);
        assert_eq!(
            summary,
            SynthesisSummary {
                ran: 5,
                batches: 2,
                fallbacks: 1,
                ..SynthesisSummary::default()
            }
        );
        assert_eq!(*runner.batch_sizes.lock().unwrap(), [3, 2]);
        assert_eq!(
            runner.single.calls.load(Ordering::SeqCst),
            1,
            "only p1 re-ran alone"
        );
        let desc = |i: usize| match &events.iter().find(|e| e.index == i).unwrap().result {
            Ok((s, Source::Agent)) => s.description.clone(),
            other => panic!("{other:?}"),
        };
        assert!(desc(0).starts_with("batch Project: p0"));
        assert!(desc(1).starts_with("Project: p1"), "fallback result");
        // Batch tokens are split per project: 90 / 3 = 30 in, 30 / 3 = 10 out.
        let p0 = store.project(1).unwrap().unwrap();
        let key = cache_key(&reports[0], &PackOptions::default());
        let cached = store.cached_synthesis(p0.id, &key).unwrap().unwrap();
        assert_eq!(
            (cached.tokens_in, cached.tokens_out, cached.model.as_deref()),
            (30, 10, Some("fake-batch"))
        );

        // A runner without batch support falls back to single calls for every item.
        let (_dir2, reports2) = setup(3);
        let plain = FakeRunner::new(None);
        let (summary, _) = run_batched(&Store::open_in_memory().unwrap(), &reports2, &plain, b);
        assert_eq!((summary.ran, summary.batches, summary.fallbacks), (3, 1, 3));
        assert_eq!(plain.calls.load(Ordering::SeqCst), 3);
    }

    fn setup(n: usize) -> (tempfile::TempDir, Vec<ProjectReport>) {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..n {
            let p = dir.path().join(format!("p{i}"));
            std::fs::create_dir_all(&p).unwrap();
            git(&p, &["init", "-q", "-b", "main"]);
            write(&p, "a.txt", "a\n");
            git(&p, &["add", "a.txt"]);
            git(&p, &["commit", "-qm", "c"]);
        }
        let config = Config {
            roots: vec![dir.path().to_owned()],
            ..Config::default()
        };
        let report = scan::scan(&config, &LogSources::default(), Utc::now()).unwrap();
        (dir, report.projects)
    }

    fn run_all(
        store: &Store,
        reports: &[ProjectReport],
        runner: &FakeRunner,
        concurrency: usize,
    ) -> (SynthesisSummary, Vec<SynthesisEvent>) {
        let jobs: Vec<Job> = reports
            .iter()
            .map(|r| Job {
                report: r,
                project_id: store
                    .upsert_project(&r.project.path, &r.project.title, Utc::now())
                    .unwrap(),
                options: PackOptions::default(),
                force: false,
            })
            .collect();
        let mut events = Vec::new();
        let summary = synthesize(store, &jobs, runner, concurrency, Utc::now(), |e| events.push(e));
        (summary, events)
    }

    #[test]
    fn cache_hits_skip_the_runner() {
        let (_dir, reports) = setup(3);
        let store = Store::open_in_memory().unwrap();
        let runner = FakeRunner::new(None);
        let (first, events) = run_all(&store, &reports, &runner, 3);
        assert_eq!(
            first,
            SynthesisSummary {
                next_md: 0,
                cached: 0,
                ran: 3,
                failed: 0,
                ..SynthesisSummary::default()
            }
        );
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| matches!(&e.result, Ok((_, Source::Agent)))));

        let (second, events) = run_all(&store, &reports, &runner, 3);
        assert_eq!(
            second,
            SynthesisSummary {
                next_md: 0,
                cached: 3,
                ran: 0,
                failed: 0,
                ..SynthesisSummary::default()
            }
        );
        assert_eq!(runner.calls.load(Ordering::SeqCst), 3, "no new agent calls");
        let first_desc = match &events[0].result {
            Ok((s, Source::Cache)) => s.description.clone(),
            other => panic!("{other:?}"),
        };
        assert!(first_desc.starts_with("Project: p0"));
    }

    #[test]
    fn current_next_md_skips_the_agent() {
        let (dir, reports) = setup(2);
        let store = Store::open_in_memory().unwrap();
        let runner = FakeRunner::new(None);
        let p0 = dir.path().join("p0");
        let written = Utc::now();
        let s = crate::synthesis::Synthesis {
            description: "From NEXT.md".into(),
            state: "s".into(),
            last_goal: "g".into(),
            next_steps: vec!["step".into()],
            blockers: vec![],
            confidence: crate::synthesis::Confidence::High,
        };
        next_md::write(&p0, &s, written, "logpose").unwrap();
        let (summary, events) = run_all(&store, &reports, &runner, 2);
        assert_eq!(
            summary,
            SynthesisSummary {
                next_md: 1,
                cached: 0,
                ran: 1,
                failed: 0,
                ..SynthesisSummary::default()
            }
        );
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        let from_file = events.iter().find(|e| e.index == 0).unwrap();
        match &from_file.result {
            Ok((s, Source::NextMd)) => assert_eq!(s.description, "From NEXT.md"),
            other => panic!("{other:?}"),
        }

        // Activity after the file was written makes it stale: the agent runs again.
        let mut later = reports.clone();
        later[0].staleness.as_mut().unwrap().last_activity = written + chrono::Duration::minutes(5);
        let (summary, _) = run_all(&store, &later, &runner, 2);
        assert_eq!((summary.next_md, summary.ran), (0, 1));
    }

    #[test]
    fn force_reruns_and_known_reports_outdated_results() {
        let (dir, reports) = setup(2);
        let store = Store::open_in_memory().unwrap();
        let runner = FakeRunner::new(None);
        run_all(&store, &reports, &runner, 2);
        fn job<'a>(store: &Store, r: &'a ProjectReport, force: bool) -> Job<'a> {
            Job {
                report: r,
                project_id: store
                    .upsert_project(&r.project.path, &r.project.title, Utc::now())
                    .unwrap(),
                options: PackOptions::default(),
                force,
            }
        }
        let k = known(&store, &job(&store, &reports[0], false)).unwrap();
        assert_eq!(
            (k.source, k.outdated, k.agent.as_deref()),
            (Source::Cache, false, Some("fake"))
        );

        // Refresh of one project: only it calls the agent, even though its cache entry is current.
        let jobs = [job(&store, &reports[0], true)];
        let summary = synthesize(&store, &jobs, &runner, 2, Utc::now(), |_| {});
        assert_eq!((summary.ran, summary.cached), (1, 0));
        assert_eq!(runner.calls.load(Ordering::SeqCst), 3);

        // After a new commit the key changes: the previous result is still shown, marked outdated.
        let p1 = dir.path().join("p1");
        write(&p1, "a.txt", "changed\n");
        git(&p1, &["commit", "-qam", "new"]);
        let config = Config {
            roots: vec![dir.path().to_owned()],
            ..Config::default()
        };
        let rescanned = scan::scan(&config, &LogSources::default(), Utc::now())
            .unwrap()
            .projects;
        let k = known(&store, &job(&store, &rescanned[1], false)).unwrap();
        assert!(k.outdated && k.synthesis.description.starts_with("Project: p1"));
        // Never synthesized: nothing known.
        let (_d, fresh) = setup(1);
        assert!(known(&store, &job(&store, &fresh[0], false)).is_none());
    }

    #[test]
    fn changes_invalidate_only_that_project() {
        let (dir, reports) = setup(2);
        let store = Store::open_in_memory().unwrap();
        let runner = FakeRunner::new(None);
        run_all(&store, &reports, &runner, 2);
        let p0 = dir.path().join("p0");
        write(&p0, "a.txt", "changed\n");
        git(&p0, &["commit", "-qam", "new commit"]);
        let config = Config {
            roots: vec![dir.path().to_owned()],
            ..Config::default()
        };
        let rescanned = scan::scan(&config, &LogSources::default(), Utc::now())
            .unwrap()
            .projects;
        let (summary, _) = run_all(&store, &rescanned, &runner, 2);
        assert_eq!(
            summary,
            SynthesisSummary {
                next_md: 0,
                cached: 1,
                ran: 1,
                failed: 0,
                ..SynthesisSummary::default()
            }
        );

        let a = cache_key(&rescanned[1], &PackOptions::default());
        let b = cache_key(
            &rescanned[1],
            &PackOptions {
                parking_note: Some("note".into()),
                ..PackOptions::default()
            },
        );
        assert_ne!(a, b, "parking note feeds the key");
    }

    #[test]
    fn concurrency_limit_and_failures() {
        let (_dir, reports) = setup(6);
        let store = Store::open_in_memory().unwrap();
        let runner = FakeRunner::new(Some("Project: p3 "));
        let (summary, events) = run_all(&store, &reports, &runner, 2);
        assert_eq!(
            summary,
            SynthesisSummary {
                next_md: 0,
                cached: 0,
                ran: 5,
                failed: 1,
                ..SynthesisSummary::default()
            }
        );
        assert_eq!(
            runner.max_running.load(Ordering::SeqCst),
            2,
            "never more than 2 concurrent runs"
        );
        assert_eq!(events.len(), 6);
        // The failure is not cached, so the next round retries only it.
        let (again, _) = run_all(&store, &reports, &runner, 2);
        assert_eq!(
            again,
            SynthesisSummary {
                next_md: 0,
                cached: 5,
                ran: 0,
                failed: 1,
                ..SynthesisSummary::default()
            }
        );
    }
}
