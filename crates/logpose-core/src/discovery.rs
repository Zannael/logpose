//! Discovery: find projects under the configured roots (BLUEPRINT §5.1, DISC-001).
//!
//! A directory is a project when it is a git repo (`.git` dir or file) or contains `CLAUDE.md` /
//! `AGENTS.md`. The walk stops descending at a project. Git repos nested inside a project are
//! collected as its sub-repos. Hidden directories, symlinked directories, and build/dependency
//! directories ([`SKIP_DIRS`]) are never entered.

use std::fs;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;

use crate::config::Config;

/// Directory names never descended into (dependency and build output).
pub const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    "venv",
    "__pycache__",
    "bower_components",
];

/// How deep below a project root to look for nested git repos.
pub const SUB_REPO_DEPTH: usize = 4;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Markers {
    pub git: bool,
    pub claude_md: bool,
    pub agents_md: bool,
}

impl Markers {
    pub fn of(dir: &Path) -> Self {
        Self {
            git: dir.join(".git").exists(),
            claude_md: dir.join("CLAUDE.md").is_file(),
            agents_md: dir.join("AGENTS.md").is_file(),
        }
    }

    pub fn is_project(&self) -> bool {
        self.git || self.claude_md || self.agents_md
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredProject {
    pub path: PathBuf,
    /// The directory name (canonical title; aliases are applied later).
    pub title: String,
    pub root: PathBuf,
    pub markers: Markers,
    /// Nested git repositories, sorted by path.
    pub sub_repos: Vec<PathBuf>,
    /// Display name from the config `[aliases]` table, if any.
    pub alias: Option<String>,
    /// Found only through an agent session `cwd` (no git, no CLAUDE.md/AGENTS.md).
    pub from_logs: bool,
}

impl DiscoveredProject {
    pub fn display_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.title)
    }
}

/// Compiled `exclude` globs, matched against absolute paths. A pattern ending in `/**` also
/// excludes the directory itself.
#[derive(Debug, Clone)]
pub struct ExcludeSet(GlobSet);

impl ExcludeSet {
    pub fn new(patterns: &[String]) -> Result<Self, globset::Error> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            builder.add(Glob::new(pattern)?);
            if let Some(dir) = pattern.strip_suffix("/**") {
                builder.add(Glob::new(dir)?);
            }
        }
        Ok(Self(builder.build()?))
    }

    pub fn is_excluded(&self, path: &Path) -> bool {
        self.0.is_match(path)
    }
}

/// Discover projects for a config: walk its roots, drop excluded projects and sub-repos, apply aliases.
pub fn discover_with_config(config: &Config) -> Result<Vec<DiscoveredProject>, globset::Error> {
    let exclude = ExcludeSet::new(&config.exclude)?;
    Ok(apply_config(
        discover(&config.roots, config.scan_depth),
        &exclude,
        config,
    ))
}

pub fn apply_config(
    found: Vec<DiscoveredProject>,
    exclude: &ExcludeSet,
    config: &Config,
) -> Vec<DiscoveredProject> {
    found
        .into_iter()
        .filter(|p| !exclude.is_excluded(&p.path))
        .map(|mut p| {
            p.sub_repos.retain(|s| !exclude.is_excluded(s));
            p.alias = config.aliases.get(&p.path).cloned();
            p
        })
        .collect()
}

/// Add agent-log working directories as projects (DISC-003). A cwd becomes a project when it is an
/// existing directory strictly inside a configured root, not inside an already-found project, not
/// excluded, and not nested in another log-only project. Cwds outside every root are ignored (DEC-003).
/// Projects already found inside such a cwd that are among its sub-repos are absorbed into it, so they
/// are not separate cards as well (DEC-002, DISC-004).
pub fn merge_log_cwds(
    mut found: Vec<DiscoveredProject>,
    cwds: impl IntoIterator<Item = PathBuf>,
    exclude: &ExcludeSet,
    config: &Config,
) -> Vec<DiscoveredProject> {
    let mut cwds: Vec<PathBuf> = cwds.into_iter().collect();
    cwds.sort();
    cwds.dedup();
    let mut added: Vec<DiscoveredProject> = Vec::new();
    for cwd in cwds {
        let Some(root) = config
            .roots
            .iter()
            .filter(|r| cwd.starts_with(r) && cwd != **r)
            .max_by_key(|r| r.components().count())
        else {
            continue;
        };
        let hidden_or_skipped = cwd.strip_prefix(root).is_ok_and(|rel| {
            rel.components().any(|c| {
                let name = c.as_os_str().to_string_lossy();
                name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref())
            })
        });
        if hidden_or_skipped
            || !cwd.is_dir()
            || exclude.is_excluded(&cwd)
            || found.iter().chain(&added).any(|p| cwd.starts_with(&p.path))
        {
            continue;
        }
        let mut sub_repos = Vec::new();
        for child in child_dirs(&cwd) {
            find_repos(&child, 1, &mut sub_repos);
        }
        sub_repos.retain(|s| !exclude.is_excluded(s));
        sub_repos.sort();
        found.retain(|p| sub_repos.binary_search(&p.path).is_err());
        added.push(DiscoveredProject {
            title: cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            alias: config.aliases.get(&cwd).cloned(),
            root: root.clone(),
            markers: Markers::of(&cwd),
            sub_repos,
            from_logs: true,
            path: cwd,
        });
    }
    found.extend(added);
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Walk every root to `max_depth` (1 = direct children of the root). The root itself can be a project.
pub fn discover(roots: &[PathBuf], max_depth: usize) -> Vec<DiscoveredProject> {
    let mut found = Vec::new();
    for root in roots {
        walk(root, root, 0, max_depth, &mut found);
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);
    found
}

fn walk(root: &Path, dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<DiscoveredProject>) {
    let markers = Markers::of(dir);
    if markers.is_project() {
        let mut sub_repos = Vec::new();
        for child in child_dirs(dir) {
            find_repos(&child, 1, &mut sub_repos);
        }
        sub_repos.sort();
        out.push(DiscoveredProject {
            title: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: dir.to_owned(),
            root: root.to_owned(),
            markers,
            sub_repos,
            alias: None,
            from_logs: false,
        });
        return;
    }
    if depth < max_depth {
        for child in child_dirs(dir) {
            walk(root, &child, depth + 1, max_depth, out);
        }
    }
}

fn find_repos(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir.join(".git").exists() {
        out.push(dir.to_owned());
    }
    if depth < SUB_REPO_DEPTH {
        for child in child_dirs(dir) {
            find_repos(&child, depth + 1, out);
        }
    }
}

/// Real (non-symlink), non-hidden, non-skipped child directories, sorted.
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref())
        })
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for p in paths {
            let full = dir.path().join(p);
            if p.ends_with('/') {
                fs::create_dir_all(&full).unwrap();
            } else {
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(&full, "").unwrap();
            }
        }
        dir
    }

    fn rel(found: &[DiscoveredProject], base: &Path) -> Vec<String> {
        found
            .iter()
            .map(|p| p.path.strip_prefix(base).unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn detection_rules_and_stop_descending() {
        let t = tree(&[
            "gitproj/.git/",
            "gitproj/inner/.git/",
            "claudeproj/CLAUDE.md",
            "agentsproj/AGENTS.md",
            "worktree/.git", // `.git` file (worktree / submodule)
            "group/nested/.git/",
            "plain/readme.txt",
        ]);
        let found = discover(&[t.path().to_owned()], 3);
        assert_eq!(
            rel(&found, t.path()),
            ["agentsproj", "claudeproj", "gitproj", "group/nested", "worktree"]
        );
        let claude = found.iter().find(|p| p.title == "claudeproj").unwrap();
        assert_eq!(
            claude.markers,
            Markers {
                git: false,
                claude_md: true,
                agents_md: false
            }
        );
        assert!(found.iter().find(|p| p.title == "worktree").unwrap().markers.git);
    }

    #[test]
    fn sub_repos_are_collected_not_listed() {
        let t = tree(&[
            "sysmod/.git/",
            "sysmod/vendor/Atmosphere/.git/",
            "sysmod/libs/a/.git/",
            "sysmod/node_modules/dep/.git/",
            "sysmod/.hidden/x/.git/",
        ]);
        let found = discover(&[t.path().to_owned()], 3);
        assert_eq!(found.len(), 1);
        let subs: Vec<_> = found[0]
            .sub_repos
            .iter()
            .map(|p| p.strip_prefix(&found[0].path).unwrap())
            .collect();
        assert_eq!(subs, [Path::new("libs/a"), Path::new("vendor/Atmosphere")]);
    }

    #[test]
    fn depth_limit_and_skip_dirs() {
        let t = tree(&[
            "a/b/c/deep/.git/",
            "a/b/shallow/.git/",
            "target/x/.git/",
            ".cache/y/.git/",
        ]);
        assert_eq!(
            rel(&discover(&[t.path().to_owned()], 3), t.path()),
            ["a/b/shallow"]
        );
        assert_eq!(
            rel(&discover(&[t.path().to_owned()], 4), t.path()),
            ["a/b/c/deep", "a/b/shallow"]
        );
    }

    #[test]
    fn root_itself_and_missing_roots() {
        let t = tree(&["CLAUDE.md", "child/.git/"]);
        let found = discover(&[t.path().to_owned(), PathBuf::from("/nonexistent/root")], 3);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, t.path());
        assert_eq!(found[0].sub_repos, [t.path().join("child")]);
    }

    #[test]
    fn exclusions_and_aliases() {
        let t = tree(&[
            "dev/work-acme/.git/",
            "dev/atac/.git/",
            "dev/keep/.git/",
            "dev/keep/vendor/big/.git/",
        ]);
        let base = t.path().to_string_lossy().into_owned();
        let mut config = Config {
            roots: vec![t.path().to_owned()],
            exclude: vec![
                "**/work-*".into(),
                format!("{base}/dev/atac/**"),
                "**/vendor/big".into(),
            ],
            ..Config::default()
        };
        config.aliases.insert(t.path().join("dev/keep"), "Keeper".into());
        let found = discover_with_config(&config).unwrap();
        assert_eq!(rel(&found, t.path()), ["dev/keep"]);
        assert_eq!(found[0].display_name(), "Keeper");
        assert_eq!(found[0].title, "keep");
        assert!(found[0].sub_repos.is_empty());

        config.exclude = vec!["[unclosed".into()];
        assert!(discover_with_config(&config).is_err());
    }

    #[test]
    fn log_cwds_become_projects_only_inside_roots() {
        let t = tree(&[
            "root/gitp/.git/",
            "root/gitp/sub/",
            "root/notes/deeper/",
            "root/work-x/",
            "outside/x/",
        ]);
        let root = t.path().join("root");
        let config = Config {
            roots: vec![root.clone()],
            exclude: vec!["**/work-*".into()],
            ..Config::default()
        };
        let exclude = ExcludeSet::new(&config.exclude).unwrap();
        let found = discover(&config.roots, 3);
        let cwds = [
            "root/gitp/sub",
            "root/notes/deeper",
            "root/notes",
            "outside/x",
            "root",
            "root/gone",
            "root/work-x",
        ]
        .map(|c| t.path().join(c));
        let merged = merge_log_cwds(found, cwds, &exclude, &config);
        assert_eq!(rel(&merged, t.path()), ["root/gitp", "root/notes"]);
        let notes = &merged[1];
        assert!(notes.from_logs && !notes.markers.is_project());
        assert_eq!(
            (notes.title.as_str(), notes.root.as_path()),
            ("notes", root.as_path())
        );
        assert!(!merged[0].from_logs);
    }

    #[test]
    fn log_only_parent_absorbs_projects_among_its_sub_repos() {
        let t = tree(&[
            "root/mobile/app/.git/",
            "root/mobile/app/CLAUDE.md",
            "root/mobile/droid/.git/",
            "root/mobile/notes/AGENTS.md",
            "root/other/.git/",
        ]);
        let root = t.path().join("root");
        let config = Config {
            roots: vec![root.clone()],
            ..Config::default()
        };
        let exclude = ExcludeSet::new(&config.exclude).unwrap();
        let found = discover(&config.roots, 3);
        assert_eq!(
            rel(&found, t.path()),
            [
                "root/mobile/app",
                "root/mobile/droid",
                "root/mobile/notes",
                "root/other"
            ]
        );
        let merged = merge_log_cwds(found, [root.join("mobile")], &exclude, &config);
        // The git children become sub-repos of the log-only parent; the non-git child stays a project.
        assert_eq!(
            rel(&merged, t.path()),
            ["root/mobile", "root/mobile/notes", "root/other"]
        );
        assert!(merged[0].from_logs);
        assert_eq!(
            merged[0].sub_repos,
            [root.join("mobile/app"), root.join("mobile/droid")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dirs_are_not_followed() {
        let t = tree(&["real/proj/.git/"]);
        std::os::unix::fs::symlink(t.path().join("real"), t.path().join("link")).unwrap();
        assert_eq!(rel(&discover(&[t.path().to_owned()], 3), t.path()), ["real/proj"]);
    }
}
