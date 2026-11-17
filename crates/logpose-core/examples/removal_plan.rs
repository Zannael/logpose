//! Print the removal plan (DEL-001) for one project of the real config. Read-only: nothing is
//! removed. Run: cargo run -p logpose-core --release --example removal_plan -- <project path>

use chrono::Utc;
use logpose_core::config::{self, Config};
use logpose_core::removal::{self, Docker};
use logpose_core::scan::{self, LogSources};
use logpose_core::sessions::{claude, codex};

fn gb(b: u64) -> String {
    format!("{:.2} GB", b as f64 / 1e9)
}

fn main() {
    let path = std::env::args().nth(1).expect("project path");
    let config = Config::load(&config::default_path().expect("config path")).expect("config");
    let report = scan::scan(&config, &LogSources::defaults(), Utc::now()).expect("scan");
    let project = report
        .projects
        .iter()
        .find(|p| p.project.path.to_string_lossy() == path)
        .expect("project in the scan");
    let logs = LogSources::defaults();
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
    let docker = removal::DockerCli::find();
    let plan = removal::plan(project, &sessions, docker.as_ref().map(|d| d as &dyn Docker));
    println!(
        "{} in {} ms; docker {:?}",
        plan.project.display(),
        plan.millis,
        plan.docker
    );
    for i in &plan.items {
        println!(
            "  [{}] {:?} {:?} {} {} — {}",
            if i.selected { "x" } else { " " },
            i.group,
            i.certainty,
            gb(i.bytes),
            i.label,
            i.detail.as_deref().unwrap_or("")
        );
        for c in &i.checks {
            println!("        $ {c}");
        }
    }
    for o in plan.build_outputs.iter().take(5) {
        println!("  build output {} {}", gb(o.bytes), o.path.display());
    }
    for r in &plan.repos {
        println!(
            "  repo {}: {:?} uncommitted {} stashes {} branches {:?}",
            r.path.display(),
            r.remotes,
            r.uncommitted,
            r.stashes,
            r.branches
        );
    }
}
