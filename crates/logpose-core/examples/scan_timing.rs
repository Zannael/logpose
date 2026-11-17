//! Where a scan spends its time on the real config (PERF-001): agent logs, discovery, and per
//! project git and mtime work (summed over projects, so above wall time when parallel), then a
//! first scan and a rescan in the same process.
//! Run: cargo run -p logpose-core --release --example scan_timing

use std::time::{Duration, Instant};

use chrono::Utc;
use logpose_core::config::{self, Config};
use logpose_core::discovery::{self, ExcludeSet};
use logpose_core::scan::{self, LogSources};
use logpose_core::sessions::{claude, codex};
use logpose_core::{git, mtime};

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    println!("{label:<28} {:>9.2?}", t.elapsed());
    out
}

fn main() {
    let config = Config::load(&config::default_path().expect("config path")).expect("config");
    let logs = LogSources::defaults();
    let (claude_sessions, _) = time("claude logs", || {
        logs.claude
            .as_deref()
            .map(claude::read_sessions)
            .unwrap_or_default()
    });
    let codex_scan = time("codex logs", || {
        logs.codex
            .as_deref()
            .map(codex::read_sessions)
            .unwrap_or_default()
    });
    println!(
        "  {} claude + {} codex sessions",
        claude_sessions.len(),
        codex_scan.sessions.len()
    );
    let exclude = ExcludeSet::new(&config.exclude).unwrap();
    let found = time("discovery", || {
        discovery::apply_config(
            discovery::discover(&config.roots, config.scan_depth),
            &exclude,
            &config,
        )
    });
    println!("  {} projects found on disk", found.len());

    let (mut sig, mut dirty, mut unmerged, mut todos, mut mt) = (
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
    );
    let mut slow: Vec<(Duration, String)> = Vec::new();
    for p in &found {
        let t0 = Instant::now();
        let mut parts = String::new();
        let repos: Vec<_> = p
            .markers
            .git
            .then(|| p.path.clone())
            .into_iter()
            .chain(p.sub_repos.iter().cloned())
            .collect();
        for r in &repos {
            let t = Instant::now();
            let s = git::scan_repo(r);
            sig += t.elapsed();
            parts += &format!(" status {:.0?}", t.elapsed());
            if let Ok(s) = s {
                let t = Instant::now();
                if let Some(d) = &s.default_branch {
                    let _ = git::unmerged_branches(r, d);
                }
                unmerged += t.elapsed();
                let t = Instant::now();
                let _ = git::dirty_stats(r);
                dirty += t.elapsed();
                parts += &format!(" dirty {:.0?}", t.elapsed());
                let t = Instant::now();
                let _ = git::todos_added(r, scan::TODO_COMMITS);
                todos += t.elapsed();
                parts += &format!(" todos {:.0?}", t.elapsed());
            }
        }
        let t = Instant::now();
        let m = mtime::scan(&p.path);
        let e = t.elapsed();
        mt += e;
        slow.push((
            t0.elapsed(),
            format!(
                "{} (mtime {:.0?}, {} files;{parts})",
                p.path.display(),
                e,
                m.files_seen
            ),
        ));
    }
    println!("sum git scan_repo            {sig:>9.2?}");
    println!("sum git unmerged_branches    {unmerged:>9.2?}");
    println!("sum git dirty_stats          {dirty:>9.2?}");
    println!("sum git todos_added          {todos:>9.2?}");
    println!("sum mtime walk               {mt:>9.2?}");
    slow.sort_by(|a, b| b.0.cmp(&a.0));
    println!("slowest projects:");
    for (d, s) in slow.iter().take(8) {
        println!("  {d:>9.2?}  {s}");
    }
    // The examples above warmed the OS file cache; the first scan below is the cold in-process case
    // (no parsed-log or TODO caches yet), the second is an app rescan.
    let r = time("full scan (first)", || {
        scan::scan(&config, &logs, Utc::now()).unwrap()
    });
    println!("  {} projects", r.projects.len());
    time("full scan (rescan)", || {
        scan::scan(&config, &logs, Utc::now()).unwrap()
    });
}
