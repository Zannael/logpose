//! Smoke test on the real agent logs. Prints counts only (no prompt or message text).
//! Run: cargo run -p logpose-core --release --example session_stats [-- ROOT...]
//! With roots, also reports discovery results and how many session cwds fall inside discovered projects.

use std::collections::HashMap;
use std::time::Instant;

use logpose_core::sessions::{claude, codex};

fn main() {
    let t = Instant::now();
    let (cc, cc_stats) = claude::read_sessions(&claude::default_base().expect("home dir"));
    println!("claude: {} sessions, {:?}, {:?}", cc.len(), cc_stats, t.elapsed());
    let t = Instant::now();
    let cx = codex::read_sessions(&codex::default_base().expect("home dir"));
    println!(
        "codex: {} threads, {:?}, {:?}",
        cx.sessions.len(),
        cx.stats,
        t.elapsed()
    );
    if let Some(rl) = &cx.rate_limit {
        println!(
            "codex rate limit: {:.1}% of {:?} min window, resets {:?}",
            rl.used_percent, rl.window_minutes, rl.resets_at
        );
    }
    let mut projects: HashMap<_, (usize, usize)> = HashMap::new();
    for s in &cc {
        projects.entry(s.cwd.clone()).or_default().0 += 1;
    }
    for s in &cx.sessions {
        projects.entry(s.cwd.clone()).or_default().1 += 1;
    }
    let both = projects.values().filter(|(a, b)| *a > 0 && *b > 0).count();
    println!(
        "distinct project cwds: {} ({} used by both agents)",
        projects.len(),
        both
    );
    let with = |v: &[logpose_core::sessions::Session]| {
        (
            v.iter().filter(|s| !s.prompts.is_empty()).count(),
            v.iter().filter(|s| s.final_message.is_some()).count(),
        )
    };
    println!("claude sessions with prompts / final message: {:?}", with(&cc));
    println!(
        "codex threads with prompts / final message: {:?}",
        with(&cx.sessions)
    );
    let tok: u64 = cc.iter().map(|s| s.tokens.total()).sum();
    let tok_cx: u64 = cx.sessions.iter().map(|s| s.tokens.total()).sum();
    println!("tokens total: claude {tok}, codex {tok_cx}");

    let roots: Vec<std::path::PathBuf> = std::env::args().skip(1).map(Into::into).collect();
    if !roots.is_empty() {
        let t = Instant::now();
        let found = logpose_core::discovery::discover(&roots, 3);
        let subs: usize = found.iter().map(|p| p.sub_repos.len()).sum();
        let git = found.iter().filter(|p| p.markers.git).count();
        println!(
            "discovery: {} projects ({} git, {} sub-repos), {:?}",
            found.len(),
            git,
            subs,
            t.elapsed()
        );
        let inside_roots: Vec<_> = projects
            .keys()
            .filter(|c| roots.iter().any(|r| c.starts_with(r)))
            .collect();
        let matched = inside_roots
            .iter()
            .filter(|c| found.iter().any(|p| c.starts_with(&p.path)))
            .count();
        println!(
            "session cwds inside roots: {}, inside a discovered project: {}",
            inside_roots.len(),
            matched
        );
        let mut kinds = std::collections::BTreeMap::new();
        for p in &found {
            *kinds
                .entry(format!("{:?}", logpose_core::launcher::detect_kind(&p.path)))
                .or_insert(0) += 1;
        }
        let config = logpose_core::config::Config {
            roots: roots.clone(),
            ..Default::default()
        };
        let exclude = logpose_core::discovery::ExcludeSet::new(&[]).unwrap();
        let cwds: Vec<_> = projects
            .keys()
            .map(|c| logpose_core::sessions::merge::normalize_path(c))
            .collect();
        let merged = logpose_core::discovery::merge_log_cwds(found.clone(), cwds, &exclude, &config);
        let from_logs = merged.iter().filter(|p| p.from_logs).count();
        println!(
            "after merging log cwds: {} projects ({from_logs} from logs only)",
            merged.len()
        );
        let t = Instant::now();
        let (mut ok, mut failed, mut dirty, mut ahead, mut no_up, mut off_default) = (0, 0, 0, 0, 0, 0);
        let (mut stashes, mut ops, mut unmerged, mut todos) = (0, 0, 0, 0);
        let (mut lines, mut old_dirty) = (0, 0);
        for p in found.iter().filter(|p| p.markers.git) {
            match logpose_core::git::scan_repo(&p.path) {
                Ok(s) => {
                    ok += 1;
                    dirty += usize::from(s.uncommitted_files() > 0);
                    ahead += usize::from(s.ahead > 0);
                    no_up += usize::from(s.no_upstream());
                    off_default += usize::from(!s.on_default_branch);
                    stashes += s.stash_count;
                    ops += usize::from(s.operation.is_some());
                    if let Some(d) = &s.default_branch {
                        unmerged += logpose_core::git::unmerged_branches(&p.path, d)
                            .map(|b| b.len())
                            .unwrap_or(0);
                    }
                    todos += logpose_core::git::todos_added(&p.path, 10).unwrap_or(0);
                    if let Ok(d) = logpose_core::git::dirty_stats(&p.path) {
                        lines += d.lines_added + d.lines_removed;
                        old_dirty += usize::from(
                            d.oldest_change_at
                                .is_some_and(|t| (chrono::Utc::now() - t).num_days() > 14),
                        );
                    }
                }
                Err(_) => failed += 1,
            }
        }
        println!(
            "git scan: {ok} ok, {failed} failed in {:?}; dirty {dirty}, ahead {ahead}, no upstream {no_up}, off default {off_default}",
            t.elapsed()
        );
        println!(
            "git extras: {stashes} stashes, {ops} ops in progress, {unmerged} unmerged branches, {todos} TODO/FIXME added (last 10 commits)"
        );
        println!(
            "dirty: {lines} changed lines total, {old_dirty} repos with an uncommitted change older than 14 days"
        );
        let cfg = logpose_core::config::OpenConfig::default();
        let opens = found
            .iter()
            .filter(|p| logpose_core::launcher::open_command(&p.path, &cfg).is_some())
            .count();
        println!(
            "project kinds: {kinds:?}; open command resolvable for {opens}/{}",
            found.len()
        );
    }
}
