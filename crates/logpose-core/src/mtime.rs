//! Latest file modification outside `.git`, respecting ignore files (GIT-005).
//!
//! Catches local work that was never committed, and is the only staleness signal for projects
//! without version control. `.gitignore` is honored even when the directory is not a git repo.
//! `NEXT.md` is skipped so Logpose's own writes don't make a project look active.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ignore::WalkBuilder;
use serde::Serialize;

use crate::discovery::SKIP_DIRS;

/// Files Logpose itself writes; never count as user activity.
pub const OWN_FILES: &[&str] = &["NEXT.md"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LatestChange {
    pub modified_at: DateTime<Utc>,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MtimeScan {
    /// `false` gives the "no VCS" badge.
    pub has_vcs: bool,
    pub latest: Option<LatestChange>,
    pub files_seen: usize,
}

pub fn scan(project: &Path) -> MtimeScan {
    let mut latest: Option<LatestChange> = None;
    let mut files_seen = 0;
    let walker = WalkBuilder::new(project)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(e.file_type().is_some_and(|t| t.is_dir())
                && (name == ".git" || SKIP_DIRS.contains(&name.as_ref())))
        })
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if entry.depth() == 1 && OWN_FILES.iter().any(|f| entry.file_name() == *f) {
            continue;
        }
        let Some(modified) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
            continue;
        };
        files_seen += 1;
        let modified_at: DateTime<Utc> = modified.into();
        if latest.as_ref().is_none_or(|l| modified_at > l.modified_at) {
            latest = Some(LatestChange {
                modified_at,
                path: entry.into_path(),
            });
        }
    }
    MtimeScan {
        has_vcs: project.join(".git").exists(),
        latest,
        files_seen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::time::{Duration, SystemTime};

    fn touch(path: &Path, secs_ago: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
        let t = SystemTime::now() - Duration::from_secs(secs_ago);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn ignored_git_own_and_dependency_files_do_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        touch(&p.join("src/main.rs"), 3600);
        touch(&p.join(".gitignore"), 7200);
        fs::write(p.join(".gitignore"), "*.log\n").unwrap();
        File::options()
            .write(true)
            .open(p.join(".gitignore"))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();
        touch(&p.join("debug.log"), 1);
        touch(&p.join(".git/index"), 1);
        touch(&p.join("NEXT.md"), 1);
        touch(&p.join("node_modules/x/index.js"), 1);
        let s = scan(p);
        assert!(s.has_vcs);
        assert_eq!(s.latest.unwrap().path, p.join("src/main.rs"));
        assert_eq!(s.files_seen, 2);
    }

    #[test]
    fn non_git_project_and_nested_next_md() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("notes.txt"), 600);
        touch(&dir.path().join("docs/NEXT.md"), 5);
        let s = scan(dir.path());
        assert!(!s.has_vcs);
        assert_eq!(s.latest.unwrap().path, dir.path().join("docs/NEXT.md"));
    }

    #[test]
    fn empty_project() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            scan(dir.path()),
            MtimeScan {
                has_vcs: false,
                latest: None,
                files_seen: 0
            }
        );
    }
}
