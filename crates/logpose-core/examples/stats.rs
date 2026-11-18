//! STATS-001/002 on the real config: authored commits over the last 365 days and the agent split
//! from session logs (counts only). Run: cargo run -p logpose-core --release --example stats [DB]
//! With a DB path (use a copy: opening migrates it), also prints the STATS-003 snapshot series.

use std::time::Instant;

use chrono::{Duration, Local, Utc};
use logpose_core::config::{self, Config};
use logpose_core::scan::{self, LogSources};
use logpose_core::sessions::{claude, codex};
use logpose_core::stats;
use logpose_core::store::Store;

fn main() {
    let config = Config::load(&config::default_path().expect("config path")).expect("config");
    let report = scan::scan(&config, &LogSources::defaults(), Utc::now()).expect("scan");
    let start = Instant::now();
    // The same repos, authors and "12 months" interval as the Stats screen.
    let pairs: Vec<_> = report
        .projects
        .iter()
        .map(|p| {
            let repos = p.repo.iter().chain(&p.sub_repos).filter(|r| r.signals.is_some());
            (p.project.path.clone(), repos.map(|r| r.path.clone()).collect())
        })
        .collect();
    let since = (Local::now().date_naive() - Duration::days(364))
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(Local).earliest())
        .expect("midnight")
        .with_timezone(&Utc);
    let authors = stats::Authors::RepoEmail(&config.stats.emails);
    let s = stats::git_stats_for(&pairs, authors, since, None, &Local);
    let commits: usize = s.commits_per_day.values().sum();
    let busiest = s.commits_per_day.iter().max_by_key(|(_, n)| **n);
    let max_week = s.projects_per_week.values().max().copied().unwrap_or(0);
    println!(
        "{} projects: {commits} authored commits on {} days in {:.2?}; busiest day {:?}; {} weeks with commits, max {max_week} projects in a week; {} emails; {} errors",
        report.projects.len(),
        s.commits_per_day.len(),
        start.elapsed(),
        busiest,
        s.projects_per_week.len(),
        s.emails.len(),
        s.errors.len(),
    );
    for (email, n) in &s.commits_by_email {
        println!("  {email}: {n} commits");
    }

    let logs = LogSources::defaults();
    let start = Instant::now();
    let mut sessions = logs
        .claude
        .as_deref()
        .map(|b| claude::read_sessions(b).0)
        .unwrap_or_default();
    sessions.extend(
        logs.codex
            .as_deref()
            .map(|b| codex::read_sessions(b).sessions)
            .unwrap_or_default(),
    );
    let targets = logpose_core::watch::targets(&report, &LogSources::defaults());
    println!(
        "watch (LATER-003): {} folders, {} recursive",
        targets.len(),
        targets.iter().filter(|(_, r)| *r).count()
    );
    let suggested = logpose_core::board::suggest(&report.projects);
    for col in logpose_core::store::BoardColumn::ALL {
        let names: Vec<String> = suggested
            .iter()
            .filter(|(_, c)| *c == col)
            .map(|(p, _)| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
            .collect();
        println!("suggested {:?} ({}): {}", col, names.len(), names.join(", "));
    }
    let m = logpose_core::usage::meters(&sessions, report.codex_rate_limit.as_ref(), None, Utc::now());
    println!(
        "meters (SYN-009): claude 7d {} sessions / {} tokens + {} cache reads; codex 7d {} sessions / {} tokens + {} cache reads; codex limit {:?}",
        m.claude_code.sessions,
        m.claude_code.tokens,
        m.claude_code.cache_read,
        m.codex.sessions,
        m.codex.tokens,
        m.codex.cache_read,
        m.codex_limit
    );
    let paths: Vec<_> = report.projects.iter().map(|p| p.project.path.clone()).collect();
    let s = stats::session_stats(&sessions, &paths, &Local);
    let both = s
        .per_project
        .values()
        .filter(|a| a.claude_code.sessions > 0 && a.codex.sessions > 0)
        .count();
    let total = |f: fn(&stats::AgentSplit) -> usize| s.per_week.values().map(f).sum::<usize>();
    println!(
        "{} sessions in {:.2?}: Claude Code {} / Codex {}; {} projects with sessions ({both} with both agents); {} weeks; unattributed {} + {}",
        sessions.len(),
        start.elapsed(),
        total(|a| a.claude_code.sessions),
        total(|a| a.codex.sessions),
        s.per_project.len(),
        s.per_week.len(),
        s.unattributed.claude_code.sessions,
        s.unattributed.codex.sessions,
    );

    if let Some(db) = std::env::args().nth(1) {
        let store = Store::open(std::path::Path::new(&db)).expect("store");
        let snaps = store
            .snapshots_since(Utc::now() - Duration::days(365))
            .expect("snapshots");
        for (day, c) in stats::bucket_series(&snaps, &Local) {
            println!("{day}: {c:?}");
        }
        let moves = stats::moves_per_month(&store.projects().expect("projects"), &Local);
        println!("{} snapshots; moves per month: {moves:?}", snaps.len());
    }
}
