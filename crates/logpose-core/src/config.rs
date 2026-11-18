//! User configuration (`config.toml`, BLUEPRINT §9).
//!
//! Location: `<platform config dir>/logpose/config.toml` (`~/.config/logpose/` on Linux). A missing file
//! means all defaults. Unknown keys are rejected so typos surface instead of being silently ignored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid config {path}: {source}")]
    Parse { path: PathBuf, source: toml::de::Error },
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Top-level folders where projects live. `~` is expanded.
    pub roots: Vec<PathBuf>,
    /// Glob patterns of paths to skip. A leading `~` is expanded.
    pub exclude: Vec<String>,
    pub scan_depth: usize,
    /// Display names keyed by project path (`~` expanded). Titles default to the directory name.
    pub aliases: BTreeMap<PathBuf, String>,
    pub agent: AgentConfig,
    pub next_md: NextMdConfig,
    pub score: ScoreConfig,
    pub open: OpenConfig,
    pub stats: StatsConfig,
}

/// Stats options (STATS-005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct StatsConfig {
    /// More author emails of the user (old or work addresses), counted together with each repo's
    /// own `user.email`.
    pub emails: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultAgent {
    Ask,
    Claude,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub default: DefaultAgent,
    pub model_claude: String,
    pub model_codex: String,
    /// Program to run for Claude Code / Codex: a name or a path; empty = find it (AGENT-001).
    pub program_claude: String,
    pub program_codex: String,
    pub concurrency: usize,
    pub context_budget_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NextMdConfig {
    pub write: bool,
    pub inject_agent_rule: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ScoreConfig {
    pub weights: ScoreWeights,
}

/// Mess-score weights (BLUEPRINT §6). Defaults sum to 100.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScoreWeights {
    pub operation_in_progress: f64,
    pub uncommitted_files: f64,
    pub oldest_uncommitted: f64,
    pub unpushed_commits: f64,
    pub stashes: f64,
    pub stale_branches: f64,
    pub no_upstream: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            exclude: Vec::new(),
            scan_depth: 3,
            aliases: BTreeMap::new(),
            agent: AgentConfig::default(),
            next_md: NextMdConfig::default(),
            score: ScoreConfig::default(),
            open: OpenConfig::default(),
            stats: StatsConfig::default(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default: DefaultAgent::Ask,
            model_claude: "haiku".into(),
            // Codex counterpart of Haiku: "Fast and affordable model for easier tasks" (2026-11-03).
            model_codex: "gpt-6-luna".into(),
            program_claude: String::new(),
            program_codex: String::new(),
            concurrency: 3,
            context_budget_tokens: 3000,
        }
    }
}

/// Programs the "Open" action starts (DEC-005). Names are looked up on `PATH`; paths are used as-is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenConfig {
    /// Editor for Python projects; empty string disables it (falls back to `directory`).
    pub python: Option<String>,
    /// Editor for other code projects.
    pub code: Option<String>,
    /// Directory opener for everything else (and the fallback).
    pub directory: String,
    /// Terminal for "Launch agent here" (LATER-002); `None` picks the first known terminal on `PATH`.
    pub terminal: Option<String>,
}

impl Default for OpenConfig {
    fn default() -> Self {
        let directory = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        Self {
            python: Some("pycharm".into()),
            code: Some("webstorm".into()),
            directory: directory.into(),
            terminal: None,
        }
    }
}

impl Default for NextMdConfig {
    fn default() -> Self {
        Self {
            write: true,
            inject_agent_rule: false,
        }
    }
}

impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            operation_in_progress: 25.0,
            uncommitted_files: 20.0,
            oldest_uncommitted: 15.0,
            unpushed_commits: 15.0,
            stashes: 10.0,
            stale_branches: 10.0,
            no_upstream: 5.0,
        }
    }
}

/// `<platform config dir>/logpose/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    crate::platform::Platform::current()
        .config_dir()
        .map(|d| d.join("logpose").join("config.toml"))
}

impl Config {
    /// Load from `path`; a missing file yields defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text).map_err(|e| match e {
                ConfigError::Parse { source, .. } => ConfigError::Parse {
                    path: path.to_owned(),
                    source,
                },
                other => other,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Parse, expand `~` using the current home directory, and validate.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let home = crate::platform::Platform::current().home();
        Self::from_toml_with_home(text, home.as_deref())
    }

    pub fn from_toml_with_home(text: &str, home: Option<&Path>) -> Result<Self, ConfigError> {
        let mut config: Config = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: PathBuf::new(),
            source,
        })?;
        config.roots = config.roots.iter().map(|p| expand_tilde(p, home)).collect();
        config.exclude = config
            .exclude
            .iter()
            .map(|g| expand_tilde(Path::new(g), home).to_string_lossy().into_owned())
            .collect();
        config.aliases = std::mem::take(&mut config.aliases)
            .into_iter()
            .map(|(p, a)| (expand_tilde(&p, home), a))
            .collect();
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut problems = Vec::new();
        if !(1..=10).contains(&self.scan_depth) {
            problems.push(format!(
                "scan_depth must be between 1 and 10, got {}",
                self.scan_depth
            ));
        }
        for root in &self.roots {
            if !root.is_absolute() {
                problems.push(format!(
                    "root must be an absolute path or start with ~: {}",
                    root.display()
                ));
            }
        }
        if self.agent.concurrency == 0 {
            problems.push("agent.concurrency must be at least 1".into());
        }
        if self.agent.context_budget_tokens < 500 {
            problems.push(format!(
                "agent.context_budget_tokens must be at least 500, got {}",
                self.agent.context_budget_tokens
            ));
        }
        let w = &self.score.weights;
        let weights = [
            w.operation_in_progress,
            w.uncommitted_files,
            w.oldest_uncommitted,
            w.unpushed_commits,
            w.stashes,
            w.stale_branches,
            w.no_upstream,
        ];
        if weights.iter().any(|x| !x.is_finite() || *x < 0.0) {
            problems.push("score.weights must be finite and non-negative".into());
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(problems.join("; ")))
        }
    }
}

/// Write `roots` into the config file at `path`, keeping every other key and comment (ONBOARD-001).
/// Paths under `home` are stored as `~/…`. The file and its directory are created if missing.
pub fn save_roots(path: &Path, roots: &[PathBuf], home: Option<&Path>) -> Result<(), ConfigError> {
    edit(path, home, |doc| {
        doc["roots"] = toml_edit::value(path_array(roots, home));
    })
}

/// What the settings screen edits (SET-001).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locations {
    pub roots: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub aliases: BTreeMap<PathBuf, String>,
}

/// Write `roots`, `exclude` and `[aliases]`, keeping every other key and comment (SET-001). Paths
/// and globs under `home` are stored as `~/…`. Alias entries that stay keep their decoration;
/// removed ones are dropped. Invalid globs are rejected before anything is written.
pub fn save_locations(path: &Path, loc: &Locations, home: Option<&Path>) -> Result<(), ConfigError> {
    crate::discovery::ExcludeSet::new(&loc.exclude)
        .map_err(|e| ConfigError::Invalid(format!("invalid exclude glob: {e}")))?;
    edit(path, home, |doc| {
        doc["roots"] = toml_edit::value(path_array(&loc.roots, home));
        let mut exclude = toml_edit::Array::new();
        for g in &loc.exclude {
            exclude.push(collapse_home(Path::new(g.trim()), home));
        }
        doc["exclude"] = toml_edit::value(exclude);
        let wanted: BTreeMap<String, &str> = loc
            .aliases
            .iter()
            .map(|(p, a)| (collapse_home(p, home), a.trim()))
            .filter(|(_, a)| !a.is_empty())
            .collect();
        if wanted.is_empty() {
            doc.remove("aliases");
            return;
        }
        if !doc.get("aliases").is_some_and(|t| t.is_table()) {
            doc["aliases"] = toml_edit::table();
        }
        let table = doc["aliases"].as_table_mut().expect("table ensured above");
        // Existing keys are compared after `~` expansion, so `~/x` and `/home/u/x` are the same alias.
        let existing: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
        for key in existing {
            let normalized = collapse_home(&expand_tilde(Path::new(&key), home), home);
            if !wanted.contains_key(&normalized) || normalized != key {
                table.remove(&key);
            }
        }
        for (key, alias) in wanted {
            table[&key] = toml_edit::value(alias);
        }
    })
}

/// Write `[agent] program_claude` / `program_codex` (AGENT-001), keeping everything else. An empty
/// value removes the key, so the program is found automatically again.
pub fn save_agent_programs(
    path: &Path,
    claude: &str,
    codex: &str,
    home: Option<&Path>,
) -> Result<(), ConfigError> {
    edit(path, home, |doc| {
        if !doc.get("agent").is_some_and(|t| t.is_table()) {
            doc["agent"] = toml_edit::table();
        }
        let table = doc["agent"].as_table_mut().expect("table ensured above");
        for (key, value) in [("program_claude", claude), ("program_codex", codex)] {
            match value.trim() {
                "" => {
                    table.remove(key);
                }
                v => table[key] = toml_edit::value(v),
            }
        }
    })
}

/// Write `[stats] emails` (STATS-005), trimmed, without blanks or case-insensitive duplicates, keeping
/// everything else. An empty list removes the key.
pub fn save_stats_emails(path: &Path, emails: &[String], home: Option<&Path>) -> Result<(), ConfigError> {
    let mut seen = std::collections::BTreeSet::new();
    let emails: Vec<&str> = emails
        .iter()
        .map(|e| e.trim())
        .filter(|e| !e.is_empty() && seen.insert(e.to_lowercase()))
        .collect();
    if let Some(bad) = emails.iter().find(|e| !e.contains('@')) {
        return Err(ConfigError::Invalid(format!("not an email address: {bad}")));
    }
    edit(path, home, |doc| {
        if emails.is_empty() {
            if let Some(t) = doc.get_mut("stats").and_then(|t| t.as_table_mut()) {
                t.remove("emails");
                if t.is_empty() {
                    doc.remove("stats");
                }
            }
            return;
        }
        if !doc.get("stats").is_some_and(|t| t.is_table()) {
            doc["stats"] = toml_edit::table();
        }
        doc["stats"]["emails"] = toml_edit::value(emails.iter().copied().collect::<toml_edit::Array>());
    })
}

/// Parse the file at `path` (missing = empty), apply `change`, validate, then write atomically.
fn edit(
    path: &Path,
    home: Option<&Path>,
    change: impl FnOnce(&mut toml_edit::DocumentMut),
) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io(e)),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| ConfigError::Invalid(format!("{}: {e}", path.display())))?;
    change(&mut doc);
    // Validate the result before writing so a bad edit never lands on disk.
    Config::from_toml_with_home(&doc.to_string(), home)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}

fn path_array(paths: &[PathBuf], home: Option<&Path>) -> toml_edit::Array {
    let mut array = toml_edit::Array::new();
    for p in paths {
        array.push(collapse_home(p, home));
    }
    array
}

/// `~/…` for paths under `home`, else the path as is.
fn collapse_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/u";

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::from_toml_with_home(text, Some(Path::new(HOME)))
    }

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
        let w = ScoreWeights::default();
        let sum = w.operation_in_progress
            + w.uncommitted_files
            + w.oldest_uncommitted
            + w.unpushed_commits
            + w.stashes
            + w.stale_branches
            + w.no_upstream;
        assert_eq!(sum, 100.0);
    }

    #[test]
    fn blueprint_example_with_overrides() {
        let c = parse(
            r#"
            roots = ["~/dev", "/srv/projects"]
            exclude = ["**/work-*", "~/dev/atac/**"]
            scan_depth = 4
            [aliases]
            "~/dev/sysmod" = "Switch sysmodule"
            [agent]
            default = "codex"
            concurrency = 5
            [next_md]
            write = false
            [score.weights]
            stashes = 0
            "#,
        )
        .unwrap();
        assert_eq!(
            c.roots,
            [PathBuf::from("/home/u/dev"), PathBuf::from("/srv/projects")]
        );
        assert_eq!(c.exclude, ["**/work-*", "/home/u/dev/atac/**"]);
        assert_eq!(c.scan_depth, 4);
        assert_eq!(c.aliases[Path::new("/home/u/dev/sysmod")], "Switch sysmodule");
        assert_eq!(c.agent.default, DefaultAgent::Codex);
        assert_eq!((c.agent.concurrency, c.agent.model_claude.as_str()), (5, "haiku"));
        assert!(!c.next_md.write && !c.next_md.inject_agent_rule);
        assert_eq!(c.score.weights.stashes, 0.0);
        assert_eq!(c.score.weights.operation_in_progress, 25.0);
    }

    #[test]
    fn invalid_input_is_reported() {
        let unknown = parse("rootz = []").unwrap_err();
        assert!(matches!(unknown, ConfigError::Parse { .. }), "{unknown}");
        assert!(matches!(
            parse("[agent]\ndefault = \"gpt\"").unwrap_err(),
            ConfigError::Parse { .. }
        ));
        let err = parse(
            "scan_depth = 0\nroots = [\"relative\"]\n[agent]\nconcurrency = 0\n[score.weights]\nstashes = -1",
        )
        .unwrap_err()
        .to_string();
        for fragment in ["scan_depth", "absolute", "concurrency", "non-negative"] {
            assert!(err.contains(fragment), "{fragment} missing from {err}");
        }
    }

    #[test]
    fn save_locations_keeps_comments_and_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let home = Path::new(HOME);
        std::fs::write(
            &path,
            "# top comment\nroots = [\"~/old\"]\nscan_depth = 3 # keep me\n\n[aliases]\n# alias comment\n\"~/dev/a\" = \"Alpha\"\n\"/home/u/dev/b\" = \"Beta\"\n\n[agent]\nconcurrency = 2\n",
        )
        .unwrap();
        let loc = Locations {
            roots: vec![PathBuf::from("/home/u/dev"), PathBuf::from("/srv/code")],
            exclude: vec!["/home/u/dev/work-*".into(), " **/vendor/** ".into()],
            aliases: BTreeMap::from([
                (PathBuf::from("/home/u/dev/a"), "Alpha 2".to_string()),
                (PathBuf::from("/home/u/dev/c"), "Gamma".to_string()),
                (PathBuf::from("/home/u/dev/empty"), "  ".to_string()),
            ]),
        };
        save_locations(&path, &loc, Some(home)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# top comment\n"));
        assert!(text.contains("scan_depth = 3 # keep me"));
        assert!(text.contains("[agent]\nconcurrency = 2"));
        assert!(text.contains(r#"roots = ["~/dev", "/srv/code"]"#));
        assert!(text.contains(r#"exclude = ["~/dev/work-*", "**/vendor/**"]"#));
        assert!(!text.contains("Beta"), "removed alias dropped");
        let c = parse(&text).unwrap();
        assert_eq!(c.roots, loc.roots);
        assert_eq!(c.scan_depth, 3);
        assert_eq!(c.agent.concurrency, 2);
        assert_eq!(
            c.aliases,
            BTreeMap::from([
                (PathBuf::from("/home/u/dev/a"), "Alpha 2".to_string()),
                (PathBuf::from("/home/u/dev/c"), "Gamma".to_string()),
            ])
        );
        // Removing every alias drops the table; a bad glob or root writes nothing.
        save_locations(
            &path,
            &Locations {
                roots: loc.roots.clone(),
                ..Default::default()
            },
            Some(home),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("[aliases]") && text.contains("[agent]"));
        let bad_glob = Locations {
            roots: loc.roots.clone(),
            exclude: vec!["[unclosed".into()],
            ..Default::default()
        };
        assert!(save_locations(&path, &bad_glob, Some(home)).is_err());
        let bad_root = Locations {
            roots: vec![PathBuf::from("relative")],
            ..Default::default()
        };
        assert!(save_locations(&path, &bad_root, Some(home)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn save_roots_creates_and_preserves() {
        let dir = tempfile::tempdir().unwrap();
        let home = Path::new(HOME);
        let path = dir.path().join("logpose/config.toml");
        save_roots(
            &path,
            &[PathBuf::from("/home/u/dev"), PathBuf::from("/srv/p")],
            Some(home),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "roots = [\"~/dev\", \"/srv/p\"]\n"
        );

        std::fs::write(
            &path,
            "# my comment\nroots = []\nscan_depth = 4 # deep\n[agent]\ndefault = \"codex\"\n",
        )
        .unwrap();
        save_roots(&path, &[PathBuf::from("/home/u/Projects")], Some(home)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my comment") && text.contains("scan_depth = 4 # deep"),
            "{text}"
        );
        let c = Config::from_toml_with_home(&text, Some(home)).unwrap();
        assert_eq!(c.roots, [PathBuf::from("/home/u/Projects")]);
        assert_eq!(c.agent.default, DefaultAgent::Codex);

        assert!(
            save_roots(&path, &[PathBuf::from("relative")], Some(home)).is_err(),
            "validated before writing"
        );
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("Projects"),
            "file untouched on error"
        );
    }

    #[test]
    fn save_stats_emails_sets_dedupes_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // `~` roots must stay valid: validating without the home rejected them.
        std::fs::write(&path, "roots = [\"~/My Projects\"] # mine\n").unwrap();
        let home = Some(Path::new("/home/u"));
        let list = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let emails = list(&[" Old@Uni.example ", "", "old@uni.example", "me@work.example"]);
        save_stats_emails(&path, &emails, home).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine"), "{text}");
        let c = Config::from_toml_with_home(&text, home).unwrap();
        assert_eq!(c.stats.emails, ["Old@Uni.example", "me@work.example"]);
        assert!(save_stats_emails(&path, &list(&["nope"]), home).is_err());
        save_stats_emails(&path, &[], home).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("stats"), "{text}");
    }

    #[test]
    fn save_agent_programs_sets_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let home = Some(Path::new("/home/u"));
        std::fs::write(&path, "roots = [\"~/p\"]\n[agent]\ndefault = \"codex\" # mine\n").unwrap();
        save_agent_programs(&path, " ~/.nvm/bin/claude ", "", home).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("default = \"codex\" # mine"), "{text}");
        let c = Config::from_toml_with_home(&text, home).unwrap();
        assert_eq!(c.agent.program_claude, "~/.nvm/bin/claude");
        assert_eq!(c.agent.program_codex, "");
        save_agent_programs(&path, "", "", home).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("program_"), "{text}");
        // A file without an [agent] table gets one.
        std::fs::write(&path, "roots = [\"/srv/p\"]\n").unwrap();
        save_agent_programs(&path, "", "/opt/codex", None).unwrap();
        let c = Config::from_toml_with_home(&std::fs::read_to_string(&path).unwrap(), None).unwrap();
        assert_eq!(c.agent.program_codex, "/opt/codex");
    }

    #[test]
    fn missing_file_is_default_and_bad_file_names_path() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Config::load(&dir.path().join("none.toml")).unwrap(),
            Config::default()
        );
        let bad = dir.path().join("config.toml");
        std::fs::write(&bad, "scan_depth = \"deep\"").unwrap();
        assert!(
            Config::load(&bad)
                .unwrap_err()
                .to_string()
                .contains("config.toml")
        );
    }
}
