//! `logpose` command line (CLI-001): run the zero-token scan without the GUI.
//!
//!     logpose scan [--json] [--persist] [--config PATH]
//!
//! `--json` prints the full `ScanReport` (includes recent prompts; stays on stdout).
//! `--persist` also writes projects and a snapshot to the app database.

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::Utc;
use logpose_core::config::{self, Config};
use logpose_core::scan::{self, LogSources, ProjectReport};
use logpose_core::store::{self, Store};

const USAGE: &str = "usage: logpose scan [--json] [--persist] [--config PATH]";

#[derive(Debug)]
struct Args {
    json: bool,
    persist: bool,
    config: Option<PathBuf>,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    if it.next().as_deref() != Some("scan") {
        return Err(USAGE.into());
    }
    let mut args = Args {
        json: false,
        persist: false,
        config: None,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => args.json = true,
            "--persist" => args.persist = true,
            "--config" => args.config = Some(it.next().ok_or("--config needs a path")?.into()),
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    Ok(args)
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("logpose: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let path = args
        .config
        .clone()
        .or_else(config::default_path)
        .ok_or("cannot determine the config path")?;
    let config = Config::load(&path).map_err(|e| e.to_string())?;
    if config.roots.is_empty() {
        return Err(format!(
            "no roots configured; add `roots = [\"~/dev\"]` to {}",
            path.display()
        ));
    }
    let report = scan::scan(&config, &LogSources::defaults(), Utc::now())
        .map_err(|e| format!("invalid exclude glob: {e}"))?;
    if args.persist {
        let db = store::default_path().ok_or("cannot determine the data directory")?;
        let store = Store::open(&db).map_err(|e| e.to_string())?;
        scan::persist(&store, &report).map_err(|e| e.to_string())?;
        eprintln!("persisted {} projects to {}", report.projects.len(), db.display());
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print_table(&report.projects);
        eprintln!(
            "{} projects scanned in {:.2?}",
            report.projects.len(),
            report.duration
        );
    }
    Ok(())
}

fn print_table(projects: &[ProjectReport]) {
    let mut rows: Vec<&ProjectReport> = projects.iter().collect();
    rows.sort_by(|a, b| {
        let days = |p: &ProjectReport| p.staleness.map_or(f64::MAX, |s| s.days);
        days(a).total_cmp(&days(b))
    });
    println!(
        "{:<32} {:>6} {:<8} {:>5} {:<17} {:<10}",
        "PROJECT", "DAYS", "BUCKET", "MESS", "LABEL", "AGENTS"
    );
    for p in rows {
        let (days, bucket) = p.staleness.map_or(("-".into(), "-".into()), |s| {
            (format!("{:.0}", s.days), format!("{:?}", s.bucket))
        });
        let (mess, label) = p.mess.map_or(("-".into(), "no VCS".into()), |m| {
            (format!("{:.0}", m.score), format!("{:?}", m.label))
        });
        let agents = p.activity.as_ref().map_or(String::new(), |a| {
            a.agents
                .iter()
                .map(|ag| format!("{ag:?}"))
                .collect::<Vec<_>>()
                .join("+")
        });
        let name: String = p.project.display_name().chars().take(32).collect();
        println!("{name:<32} {days:>6} {bucket:<8} {mess:>5} {label:<17} {agents:<10}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn argument_parsing() {
        let a = parse(&["scan", "--json", "--config", "/c.toml"]).unwrap();
        assert!(a.json && !a.persist);
        assert_eq!(a.config, Some(PathBuf::from("/c.toml")));
        assert!(parse(&["scan", "--persist"]).unwrap().persist);
        assert!(parse(&[]).is_err());
        assert!(parse(&["status"]).is_err());
        assert!(parse(&["scan", "--config"]).is_err());
        assert!(
            parse(&["scan", "--bogus"])
                .unwrap_err()
                .contains("unknown argument")
        );
    }

    #[test]
    fn empty_roots_are_rejected() {
        let dir = std::env::temp_dir().join(format!("logpose-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let err = run(&Args {
            json: false,
            persist: false,
            config: Some(cfg),
        })
        .unwrap_err();
        assert!(err.contains("no roots configured"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
