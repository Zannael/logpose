//! Merge sessions from all agents per project path (SESS-003).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::Session;

/// Normalize a session cwd so the same directory from different agents compares equal:
/// resolve symlinks when the path exists, otherwise normalize lexically (`.`, `..`, trailing `/`).
pub fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Group sessions of every agent by normalized project path.
pub fn group_by_project<'a>(
    sessions: impl IntoIterator<Item = &'a Session>,
) -> BTreeMap<PathBuf, Vec<&'a Session>> {
    let mut groups: BTreeMap<PathBuf, Vec<&Session>> = BTreeMap::new();
    for session in sessions {
        groups
            .entry(normalize_path(&session.cwd))
            .or_default()
            .push(session);
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{Agent, claude, codex, fixtures, summarize, utc};

    #[test]
    fn lexical_normalization() {
        let p = normalize_path(Path::new("/nonexistent/a/./b/../c/"));
        assert_eq!(p, PathBuf::from("/nonexistent/a/c"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_resolve_to_the_same_project() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        assert_eq!(normalize_path(&dir.path().join("link")), normalize_path(&real));
    }

    #[test]
    fn fixture_projects_merge_across_agents() {
        let (cc, _) = claude::read_sessions(&fixtures().join("claude"));
        let cx = codex::read_sessions(&fixtures().join("codex"));
        let groups = group_by_project(cc.iter().chain(cx.sessions.iter()));
        assert_eq!(groups.len(), 2);
        let now = utc("2026-10-31T00:00:00Z");

        let mine = summarize(&groups[Path::new("/home/user/dev/My Project")], now, 5).unwrap();
        assert_eq!(mine.agents, vec![Agent::ClaudeCode, Agent::Codex]);
        assert_eq!(mine.last_agent, Agent::Codex);
        assert_eq!(mine.last_session_at, utc("2026-10-30T15:00:31.000Z"));

        let other = summarize(&groups[Path::new("/home/user/dev/other")], now, 5).unwrap();
        assert_eq!(other.last_agent, Agent::Codex);
        assert_eq!(other.last_session_at, utc("2026-09-13T16:01:00.000Z"));
        assert_eq!(
            other.final_message.as_deref(),
            Some("The test depends on wall-clock time.")
        );
    }
}
