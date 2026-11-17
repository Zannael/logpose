//! Lightweight change watching for the optional tray mode (LATER-003).
//!
//! Only cheap, non-recursive watches: each project root and each repository's `.git` directory
//! (commits, staging, branch switches touch `HEAD`/`index`/`ORIG_HEAD` there), plus the agent log
//! folders recursively (few directories). Events are debounced: a rescan is signalled after
//! [`QUIET`] without new events, and at most once per [`MIN_INTERVAL`], because agent logs are
//! written continuously while a session runs.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::scan::{LogSources, ScanReport};

pub const QUIET: Duration = Duration::from_secs(5);
pub const MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Paths to watch and whether recursively.
pub fn targets(report: &ScanReport, logs: &LogSources) -> Vec<(PathBuf, bool)> {
    let mut out: Vec<(PathBuf, bool)> = Vec::new();
    for p in &report.projects {
        out.push((p.project.path.clone(), false));
        for repo in p.repo.iter().chain(&p.sub_repos) {
            let git = repo.path.join(".git");
            if git.is_dir() {
                out.push((git, false));
            }
        }
    }
    if let Some(c) = &logs.claude {
        out.push((c.join("projects"), true));
    }
    if let Some(c) = &logs.codex {
        out.push((c.join("sessions"), true));
    }
    out.retain(|(p, _)| p.is_dir());
    out.sort();
    out.dedup();
    out
}

/// Debounce state: when to signal a rescan.
#[derive(Debug, Clone, Copy)]
pub struct Debounce {
    pending_since: Option<Instant>,
    last_event: Option<Instant>,
    last_fire: Option<Instant>,
}

impl Debounce {
    pub fn new() -> Self {
        Self {
            pending_since: None,
            last_event: None,
            last_fire: None,
        }
    }

    pub fn event(&mut self, now: Instant) {
        self.pending_since.get_or_insert(now);
        self.last_event = Some(now);
    }

    /// True (once) when events are pending, quiet for [`QUIET`], and the last signal is at least
    /// [`MIN_INTERVAL`] old.
    pub fn poll(&mut self, now: Instant) -> bool {
        let (Some(_), Some(last)) = (self.pending_since, self.last_event) else {
            return false;
        };
        let quiet = now.duration_since(last) >= QUIET;
        let spaced = self
            .last_fire
            .is_none_or(|f| now.duration_since(f) >= MIN_INTERVAL);
        if quiet && spaced {
            self.pending_since = None;
            self.last_fire = Some(now);
            return true;
        }
        false
    }
}

impl Default for Debounce {
    fn default() -> Self {
        Self::new()
    }
}

/// Watching in progress; dropping it stops the watcher and its thread.
pub struct WatchHandle {
    _watcher: RecommendedWatcher,
    stop: mpsc::Sender<()>,
    pub watched: usize,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

/// Start watching `targets`; `on_change` runs on a background thread after each debounced burst.
/// Paths that cannot be watched are skipped.
pub fn start(
    targets: &[(PathBuf, bool)],
    on_change: impl Fn() + Send + 'static,
) -> notify::Result<WatchHandle> {
    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(e) = res
            && !e.kind.is_access()
            && !e.paths.iter().all(|p| is_noise(p))
        {
            let _ = tx.send(());
        }
    })?;
    let mut watched = 0;
    for (path, recursive) in targets {
        let mode = if *recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        if watcher.watch(path, mode).is_ok() {
            watched += 1;
        }
    }
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut d = Debounce::new();
        loop {
            if stop_rx.try_recv().is_ok() {
                return;
            }
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(()) => d.event(Instant::now()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if d.poll(Instant::now()) {
                on_change();
            }
        }
    });
    Ok(WatchHandle {
        _watcher: watcher,
        stop: stop_tx,
        watched,
    })
}

/// Changes that never mean project activity: Logpose's own `NEXT.md`, git lock files.
fn is_noise(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    name == "NEXT.md" || name.ends_with(".lock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::git::testutil::{git, write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn debounce_waits_for_quiet_and_spaces_signals() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut d = Debounce::new();
        assert!(!d.poll(s(0)), "nothing pending");
        d.event(s(0));
        d.event(s(3));
        assert!(!d.poll(s(6)), "only 3 s quiet");
        assert!(d.poll(s(8)), "5 s quiet");
        assert!(!d.poll(s(9)), "fires once");
        d.event(s(20));
        assert!(!d.poll(s(30)), "less than 60 s since the last signal");
        assert!(d.poll(s(68)));
    }

    #[test]
    fn targets_are_roots_git_dirs_and_log_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let a = root.join("a");
        std::fs::create_dir_all(&a).unwrap();
        git(&a, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("notes")).unwrap();
        write(&root.join("notes"), "CLAUDE.md", "x");
        let logs_dir = dir.path().join("claude");
        std::fs::create_dir_all(logs_dir.join("projects")).unwrap();
        let config = Config {
            roots: vec![root.clone()],
            ..Config::default()
        };
        let report = crate::scan::scan(&config, &LogSources::default(), chrono::Utc::now()).unwrap();
        let logs = LogSources {
            claude: Some(logs_dir.clone()),
            codex: Some(dir.path().join("missing-codex")),
        };
        let t = targets(&report, &logs);
        assert_eq!(
            t,
            [
                (logs_dir.join("projects"), true),
                (a.clone(), false),
                (a.join(".git"), false),
                (root.join("notes"), false),
            ]
        );
    }

    #[test]
    fn a_change_signals_after_the_quiet_period() {
        let dir = tempfile::tempdir().unwrap();
        let fired = Arc::new(AtomicUsize::new(0));
        let f = fired.clone();
        let handle = start(&[(dir.path().to_owned(), false)], move || {
            f.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        assert_eq!(handle.watched, 1);
        std::fs::write(dir.path().join("NEXT.md"), "ignored").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        let deadline = Instant::now() + QUIET + Duration::from_secs(4);
        while fired.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        drop(handle);
    }
}
