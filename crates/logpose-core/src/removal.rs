//! Project removal (DEL-001, DEL-002; decisions in DEC-009).
//!
//! [`plan`] lists everything that belongs to one project, with sizes, and changes nothing:
//! the folder (always moved to the Trash) with its build outputs, the Docker containers whose
//! compose working directory is inside it (plus their compose networks), the volumes and images of
//! those compose projects, the agent session logs for the folder, and Logpose's own data. Docker
//! items matched only by a compose project name that a compose file in the folder would use are
//! "probable": unticked, with copyable commands to check them. [`execute`] runs the ticked items in
//! a safe order and stops before touching the folder if anything earlier failed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::discovery::SKIP_DIRS;
use crate::git::{self, BranchState, RemoteInfo};
use crate::scan::ProjectReport;
use crate::sessions::merge::normalize_path;
use crate::sessions::{Agent, Session};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Group {
    Folder,
    Containers,
    Volumes,
    Images,
    AgentLogs,
    Logpose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Certainty {
    /// Matched by path, or by a label that a container in this folder confirmed.
    Certain,
    /// Matched only by the compose project name a compose file here would use.
    Probable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Action {
    /// Move to the desktop Trash (never deleted permanently, DEC-009).
    Trash {
        paths: Vec<PathBuf>,
    },
    Container {
        id: String,
    },
    Network {
        id: String,
    },
    Volume {
        name: String,
    },
    Image {
        id: String,
    },
    /// Drop the project from Logpose's database (done by the caller).
    Forget,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub id: String,
    pub group: Group,
    pub label: String,
    pub detail: Option<String>,
    /// Bytes freed (or moved to the Trash) by this item.
    pub bytes: u64,
    pub certainty: Certainty,
    /// Ticked by default.
    pub selected: bool,
    /// Always done; cannot be unticked (the folder and Logpose's data).
    pub required: bool,
    /// Commands the user can copy to check the item before ticking it.
    pub checks: Vec<String>,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildOutput {
    pub path: PathBuf,
    pub bytes: u64,
}

/// What `git clone` would and would not bring back for one repository of the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoRecovery {
    pub path: PathBuf,
    pub remotes: Vec<RemoteInfo>,
    pub branches: Vec<BranchState>,
    /// Staged, modified, untracked and conflicted files.
    pub uncommitted: usize,
    pub stashes: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DockerStatus {
    Available,
    /// No `docker` program on `PATH`.
    Missing,
    /// The program exists but the daemon could not be queried.
    Unreachable {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plan {
    pub project: PathBuf,
    pub items: Vec<Item>,
    pub build_outputs: Vec<BuildOutput>,
    pub repos: Vec<RepoRecovery>,
    pub docker: DockerStatus,
    pub millis: u64,
}

/// Runs `docker <args>` and returns stdout, or the error text.
pub trait Docker: Sync {
    fn run(&self, args: &[String]) -> Result<String, String>;
}

pub struct DockerCli {
    pub program: PathBuf,
}

impl DockerCli {
    pub fn find() -> Option<Self> {
        crate::launcher::find_program("docker", std::env::var_os("PATH").as_deref())
            .map(|program| Self { program })
    }
}

impl Docker for DockerCli {
    fn run(&self, args: &[String]) -> Result<String, String> {
        let out = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

// ---------------------------------------------------------------- sizes

/// Folder names whose contents can be rebuilt; reported separately inside the folder size.
pub const BUILD_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "__pycache__",
    ".gradle",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".parcel-cache",
    ".turbo",
];

fn disk_bytes(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Disk usage of `path` (symlinks not followed); build-output folders found below it (outside other
/// build outputs) are added to `outs`.
fn du(path: &Path, outs: &mut Vec<BuildOutput>, in_build: bool) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    let mut total = disk_bytes(&meta);
    if !meta.is_dir() {
        return total;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return total;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let is_build = !in_build
            && entry.file_type().is_ok_and(|t| t.is_dir())
            && BUILD_DIRS.contains(&entry.file_name().to_string_lossy().as_ref());
        let size = du(&child, outs, in_build || is_build);
        if is_build {
            outs.push(BuildOutput {
                path: child,
                bytes: size,
            });
        }
        total += size;
    }
    total
}

/// Folder disk usage (top-level entries measured in parallel) and its build outputs, largest first.
pub fn folder_size(path: &Path) -> (u64, Vec<BuildOutput>) {
    let children: Vec<PathBuf> = std::fs::read_dir(path)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let own = std::fs::symlink_metadata(path)
        .map(|m| disk_bytes(&m))
        .unwrap_or(0);
    let parts = crate::scan::parallel_map(&children, |_, child| {
        let mut outs = Vec::new();
        let is_build = std::fs::symlink_metadata(child).is_ok_and(|m| m.is_dir())
            && child
                .file_name()
                .is_some_and(|n| BUILD_DIRS.contains(&n.to_string_lossy().as_ref()));
        let size = du(child, &mut outs, is_build);
        if is_build {
            outs.push(BuildOutput {
                path: child.clone(),
                bytes: size,
            });
        }
        (size, outs)
    });
    let mut total = own;
    let mut outs = Vec::new();
    for (size, o) in parts {
        total += size;
        outs.extend(o);
    }
    outs.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    (total, outs)
}

/// Docker's human sizes (`14.9GB`, `108kB`, `0B`; decimal units, as `docker system df` prints them).
pub fn parse_docker_size(s: &str) -> u64 {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let Ok(n) = num.trim().parse::<f64>() else {
        return 0;
    };
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024f64.powi(2),
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        _ => return 0,
    };
    (n * mult).round() as u64
}

// ---------------------------------------------------------------- compose project names

const COMPOSE_FILES: &[&str] = &[
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// Compose's project-name normalization: lowercase, only `a-z0-9_-`, starting with a letter or digit.
pub fn normalize_compose_name(s: &str) -> String {
    let lower: String = s
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .collect();
    lower.trim_start_matches(['_', '-']).to_string()
}

/// The project name compose would use for a compose file: its top-level `name:`, else
/// `COMPOSE_PROJECT_NAME` from the `.env` next to it, else the folder name.
fn compose_name(file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let unquote = |v: &str| v.trim().trim_matches(['"', '\'']).to_string();
    let top = text.lines().find_map(|l| {
        let v = l.strip_prefix("name:")?;
        let v = v.split('#').next().unwrap_or_default();
        Some(unquote(v)).filter(|v| !v.is_empty() && !v.contains("${"))
    });
    let dir = file.parent()?;
    let env = || {
        std::fs::read_to_string(dir.join(".env"))
            .ok()?
            .lines()
            .find_map(|l| {
                Some(unquote(l.trim().strip_prefix("COMPOSE_PROJECT_NAME=")?)).filter(|v| !v.is_empty())
            })
    };
    let name = top
        .or_else(env)
        .or_else(|| Some(dir.file_name()?.to_string_lossy().into_owned()))?;
    Some(normalize_compose_name(&name)).filter(|n| !n.is_empty())
}

/// Compose project names of the compose files in `project` (up to three folders deep).
pub fn compose_names(project: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, depth: usize, out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(t) = entry.file_type() else { continue };
            if t.is_file() && COMPOSE_FILES.contains(&name.as_str()) {
                out.extend(compose_name(&entry.path()));
            } else if t.is_dir()
                && depth < 3
                && !name.starts_with('.')
                && !SKIP_DIRS.contains(&name.as_str())
                && !BUILD_DIRS.contains(&name.as_str())
            {
                walk(&entry.path(), depth + 1, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(project, 0, &mut out);
    out
}

// ---------------------------------------------------------------- docker inventory

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DContainer {
    pub id: String,
    pub name: String,
    pub image_id: String,
    pub image: String,
    pub state: String,
    pub working_dir: Option<PathBuf>,
    pub project: Option<String>,
    pub volumes: Vec<String>,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DImage {
    pub id: String,
    pub tags: Vec<String>,
    pub project: Option<String>,
    pub unique: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DVolume {
    pub name: String,
    pub project: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DNetwork {
    pub id: String,
    pub name: String,
    pub project: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inventory {
    pub containers: Vec<DContainer>,
    pub images: Vec<DImage>,
    pub volumes: Vec<DVolume>,
    pub networks: Vec<DNetwork>,
}

const PROJECT_LABEL: &str = "com.docker.compose.project";
const WORKDIR_LABEL: &str = "com.docker.compose.project.working_dir";

fn label(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn ids(out: &str) -> Vec<String> {
    let mut v: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    v.sort();
    v.dedup();
    v
}

fn inspect(d: &dyn Docker, what: &str, ids: &[String]) -> Result<Vec<Value>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec![what.to_string(), "inspect".to_string()];
    args.extend(ids.iter().cloned());
    let out = d.run(&args)?;
    serde_json::from_str::<Vec<Value>>(&out).map_err(|e| format!("docker {what} inspect: {e}"))
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// Everything Docker knows, from `system df -v` (sizes) and `inspect` (exact labels and mounts).
pub fn inventory(d: &dyn Docker) -> Result<Inventory, String> {
    let a = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let df: Value = serde_json::from_str(&d.run(&a(&["system", "df", "-v", "--format", "json"]))?)
        .map_err(|e| format!("docker system df: {e}"))?;
    let sizes = |key: &str, id: &str, size: &str| -> BTreeMap<String, u64> {
        df.get(key)
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|r| (s(r, id), parse_docker_size(&s(r, size))))
                    .collect()
            })
            .unwrap_or_default()
    };
    let image_sizes = sizes("Images", "ID", "UniqueSize");
    let volume_sizes = sizes("Volumes", "Name", "Size");
    let container_sizes = sizes("Containers", "ID", "Size");

    let mut inv = Inventory::default();
    for c in inspect(d, "container", &ids(&d.run(&a(&["ps", "-aq", "--no-trunc"]))?))? {
        let labels = &c["Config"]["Labels"];
        let id = s(&c, "Id");
        inv.containers.push(DContainer {
            size: container_sizes.get(&id).copied().unwrap_or(0),
            name: s(&c, "Name").trim_start_matches('/').to_string(),
            image_id: s(&c, "Image"),
            image: s(&c["Config"], "Image"),
            state: s(&c["State"], "Status"),
            working_dir: label(labels, WORKDIR_LABEL).map(PathBuf::from),
            project: label(labels, PROJECT_LABEL),
            volumes: c["Mounts"]
                .as_array()
                .map(|m| {
                    m.iter()
                        .filter(|m| m["Type"] == "volume")
                        .map(|m| s(m, "Name"))
                        .collect()
                })
                .unwrap_or_default(),
            id,
        });
    }
    for i in inspect(
        d,
        "image",
        &ids(&d.run(&a(&["image", "ls", "-aq", "--no-trunc"]))?),
    )? {
        let id = s(&i, "Id");
        inv.images.push(DImage {
            unique: image_sizes.get(&id).copied().unwrap_or(0),
            tags: i["RepoTags"]
                .as_array()
                .map(|t| t.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default(),
            project: label(&i["Config"]["Labels"], PROJECT_LABEL),
            id,
        });
    }
    for v in inspect(d, "volume", &ids(&d.run(&a(&["volume", "ls", "-q"]))?))? {
        let name = s(&v, "Name");
        inv.volumes.push(DVolume {
            size: volume_sizes.get(&name).copied().unwrap_or(0),
            project: label(&v["Labels"], PROJECT_LABEL),
            name,
        });
    }
    for n in inspect(
        d,
        "network",
        &ids(&d.run(&a(&["network", "ls", "-q", "--no-trunc"]))?),
    )? {
        inv.networks.push(DNetwork {
            id: s(&n, "Id"),
            name: s(&n, "Name"),
            project: label(&n["Labels"], PROJECT_LABEL),
        });
    }
    Ok(inv)
}

fn short(id: &str) -> &str {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    &id[..id.len().min(12)]
}

/// Docker items for `project` (see the module docs for the rules).
pub fn docker_items(inv: &Inventory, project: &Path, file_names: &BTreeSet<String>) -> Vec<Item> {
    let root = normalize_path(project);
    let mine: Vec<&DContainer> = inv
        .containers
        .iter()
        .filter(|c| {
            c.working_dir
                .as_ref()
                .is_some_and(|w| normalize_path(w).starts_with(&root))
        })
        .collect();
    let mine_ids: BTreeSet<&str> = mine.iter().map(|c| c.id.as_str()).collect();
    let confirmed: BTreeSet<&str> = mine.iter().filter_map(|c| c.project.as_deref()).collect();
    let probable: BTreeSet<&str> = file_names
        .iter()
        .map(String::as_str)
        .filter(|n| !confirmed.contains(n))
        .collect();
    let others = || {
        inv.containers
            .iter()
            .filter(|c| !mine_ids.contains(c.id.as_str()))
    };
    let other_volumes: BTreeSet<&str> = others()
        .flat_map(|c| c.volumes.iter().map(String::as_str))
        .collect();
    let other_images: BTreeSet<&str> = others().map(|c| c.image_id.as_str()).collect();
    let my_volumes: BTreeMap<&str, &str> = mine
        .iter()
        .flat_map(|c| c.volumes.iter().map(move |v| (v.as_str(), c.name.as_str())))
        .collect();
    let my_images: BTreeMap<&str, &str> = mine
        .iter()
        .map(|c| (c.image_id.as_str(), c.name.as_str()))
        .collect();

    let mut items = Vec::new();
    for c in &mine {
        items.push(Item {
            id: format!("container:{}", c.id),
            group: Group::Containers,
            label: c.name.clone(),
            detail: Some(format!("{} · image {}", c.state, c.image)),
            bytes: c.size,
            certainty: Certainty::Certain,
            selected: true,
            required: false,
            checks: Vec::new(),
            action: Action::Container { id: c.id.clone() },
        });
    }
    for n in &inv.networks {
        if n.project.as_deref().is_some_and(|p| confirmed.contains(p)) {
            items.push(Item {
                id: format!("network:{}", n.id),
                group: Group::Containers,
                label: format!("network {}", n.name),
                detail: Some("compose network of these containers".into()),
                bytes: 0,
                certainty: Certainty::Certain,
                selected: true,
                required: false,
                checks: Vec::new(),
                action: Action::Network { id: n.id.clone() },
            });
        }
    }
    for v in &inv.volumes {
        if other_volumes.contains(v.name.as_str()) {
            continue;
        }
        let label = v.project.as_deref();
        let (certainty, detail) = if let Some(c) = my_volumes.get(v.name.as_str()) {
            (Certainty::Certain, format!("mounted by {c}"))
        } else if let Some(p) = label.filter(|p| confirmed.contains(p)) {
            (
                Certainty::Certain,
                format!("compose project “{p}”, confirmed by a container in this folder"),
            )
        } else if let Some(p) = label.filter(|p| probable.contains(p)) {
            (
                Certainty::Probable,
                format!(
                    "compose project “{p}”: the name a compose file in this folder would use, but no container confirms it"
                ),
            )
        } else {
            continue;
        };
        items.push(Item {
            id: format!("volume:{}", v.name),
            group: Group::Volumes,
            label: v.name.clone(),
            detail: Some(detail),
            bytes: v.size,
            selected: certainty == Certainty::Certain,
            checks: if certainty == Certainty::Probable {
                vec![
                    format!("docker volume inspect {}", v.name),
                    format!("docker ps -a --filter volume={}", v.name),
                    format!("docker run --rm -v {}:/data alpine ls -la /data", v.name),
                ]
            } else {
                Vec::new()
            },
            certainty,
            required: false,
            action: Action::Volume { name: v.name.clone() },
        });
    }
    for i in &inv.images {
        if other_images.contains(i.id.as_str()) {
            continue;
        }
        let label = i.project.as_deref();
        let (certainty, detail) = if let Some(c) = my_images.get(i.id.as_str()) {
            let how = if label.is_some_and(|p| confirmed.contains(p)) {
                "built for this project"
            } else {
                "pulled from a registry (can be downloaded again)"
            };
            (Certainty::Certain, format!("{how}; used only by {c}"))
        } else if let Some(p) = label.filter(|p| confirmed.contains(p)) {
            (
                Certainty::Certain,
                format!("built for compose project “{p}”; no container uses it"),
            )
        } else if let Some(p) = label.filter(|p| probable.contains(p)) {
            (
                Certainty::Probable,
                format!(
                    "built for compose project “{p}”: the name a compose file in this folder would use, but no container confirms it"
                ),
            )
        } else {
            continue;
        };
        items.push(Item {
            id: format!("image:{}", i.id),
            group: Group::Images,
            label: if i.tags.is_empty() {
                format!("<untagged> {}", short(&i.id))
            } else {
                i.tags.join(", ")
            },
            detail: Some(detail),
            bytes: i.unique,
            selected: certainty == Certainty::Certain,
            checks: if certainty == Certainty::Probable {
                vec![
                    format!(
                        "docker image inspect {} --format '{{{{json .Config.Labels}}}}'",
                        short(&i.id)
                    ),
                    format!("docker image history {}", short(&i.id)),
                ]
            } else {
                Vec::new()
            },
            certainty,
            required: false,
            action: Action::Image { id: i.id.clone() },
        });
    }
    items
}

// ---------------------------------------------------------------- agent logs

/// Agent log items for sessions whose working directory is inside `project` (unticked: DEC-009).
pub fn log_items(project: &Path, sessions: &[Session]) -> Vec<Item> {
    let root = normalize_path(project);
    let inside: Vec<&Session> = sessions
        .iter()
        .filter(|s| normalize_path(&s.cwd).starts_with(&root))
        .collect();
    let mut items = Vec::new();
    for agent in [Agent::ClaudeCode, Agent::Codex] {
        let mine: Vec<&Session> = inside.iter().copied().filter(|s| s.agent == agent).collect();
        if mine.is_empty() {
            continue;
        }
        let files: BTreeSet<PathBuf> = mine.iter().flat_map(|s| s.files.iter().cloned()).collect();
        let mut paths: Vec<PathBuf> = Vec::new();
        if agent == Agent::ClaudeCode {
            // A session's file comes with a folder of the same name (tool results, subagents). When
            // every entry of the project folder in ~/.claude/projects belongs to these sessions, the
            // whole folder goes.
            let mut by_dir: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
            for f in &files {
                if let Some(dir) = f.parent() {
                    by_dir.entry(dir.to_owned()).or_default().push(f.clone());
                }
            }
            for (dir, fs) in by_dir {
                let mut own: BTreeSet<PathBuf> = fs.iter().cloned().collect();
                own.extend(fs.iter().map(|f| f.with_extension("")).filter(|p| p.is_dir()));
                let all_mine = std::fs::read_dir(&dir)
                    .map(|r| r.flatten().all(|e| own.contains(&e.path())))
                    .unwrap_or(false);
                if all_mine {
                    paths.push(dir);
                } else {
                    paths.extend(own);
                }
            }
        } else {
            paths.extend(files.iter().cloned());
        }
        let bytes = paths.iter().map(|p| du(p, &mut Vec::new(), true)).sum();
        let (name, what) = match agent {
            Agent::ClaudeCode => ("Claude Code", "claude"),
            Agent::Codex => ("Codex", "codex"),
        };
        items.push(Item {
            id: format!("logs:{what}"),
            group: Group::AgentLogs,
            label: format!(
                "{name}: {} session{} ({} file{})",
                mine.len(),
                if mine.len() == 1 { "" } else { "s" },
                paths.len(),
                if paths.len() == 1 { "" } else { "s" },
            ),
            detail: Some("Kept unless you tick it. Moving them to the Trash also removes them from Logpose's history and Stats.".into()),
            bytes,
            certainty: Certainty::Certain,
            selected: false,
            required: false,
            checks: Vec::new(),
            action: Action::Trash { paths },
        });
    }
    items
}

// ---------------------------------------------------------------- plan

fn recovery(report: &ProjectReport) -> Vec<RepoRecovery> {
    report
        .repo
        .iter()
        .chain(&report.sub_repos)
        .map(|r| {
            let (remotes, branches, error) = match git::remotes_and_branches(&r.path) {
                Ok((rem, br)) => (rem, br, r.error.clone()),
                Err(e) => (Vec::new(), Vec::new(), Some(e.to_string())),
            };
            RepoRecovery {
                path: r.path.clone(),
                remotes,
                branches,
                uncommitted: r.signals.as_ref().map_or(0, |s| s.uncommitted_files()),
                stashes: r.signals.as_ref().map_or(0, |s| s.stash_count),
                error,
            }
        })
        .collect()
}

/// Everything that removing `report`'s project would touch. Changes nothing.
pub fn plan(report: &ProjectReport, sessions: &[Session], docker: Option<&dyn Docker>) -> Plan {
    let start = Instant::now();
    let project = report.project.path.clone();
    let ((size, build_outputs), (docker_status, docker), repos, logs) = std::thread::scope(|scope| {
        let size = scope.spawn(|| folder_size(&project));
        let dock = scope.spawn(|| match docker {
            None => (DockerStatus::Missing, Vec::new()),
            Some(d) => match inventory(d) {
                Ok(inv) => (
                    DockerStatus::Available,
                    docker_items(&inv, &project, &compose_names(&project)),
                ),
                Err(message) => (DockerStatus::Unreachable { message }, Vec::new()),
            },
        });
        let repos = recovery(report);
        let logs = log_items(&project, sessions);
        (
            size.join().unwrap_or_default(),
            dock.join().unwrap_or((DockerStatus::Missing, Vec::new())),
            repos,
            logs,
        )
    });
    let mut items = vec![Item {
        id: "folder".into(),
        group: Group::Folder,
        label: project.display().to_string(),
        detail: Some("Moved to the Trash; restore it from the file manager if needed.".into()),
        bytes: size,
        certainty: Certainty::Certain,
        selected: true,
        required: true,
        checks: Vec::new(),
        action: Action::Trash {
            paths: vec![project.clone()],
        },
    }];
    items.extend(docker);
    items.extend(logs);
    items.push(Item {
        id: "logpose".into(),
        group: Group::Logpose,
        label: "Board card, parking note, cached summaries and scan history".into(),
        detail: Some("Always cleared.".into()),
        bytes: 0,
        certainty: Certainty::Certain,
        selected: true,
        required: true,
        checks: Vec::new(),
        action: Action::Forget,
    });
    Plan {
        project,
        items,
        build_outputs,
        repos,
        docker: docker_status,
        millis: start.elapsed().as_millis() as u64,
    }
}

// ---------------------------------------------------------------- execution

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepResult {
    pub id: String,
    pub ok: bool,
    pub error: Option<String>,
    /// Bytes freed or moved to the Trash by this step (0 when it failed).
    pub bytes: u64,
    /// True when the bytes went to the Trash (freed only when the Trash is emptied).
    pub trashed: bool,
}

fn order(a: &Action, group: Group) -> u8 {
    match (a, group) {
        (Action::Container { .. }, _) => 0,
        (Action::Network { .. }, _) => 1,
        (Action::Volume { .. }, _) => 2,
        (Action::Image { .. }, _) => 3,
        (Action::Trash { .. }, Group::Folder) => 5,
        (Action::Trash { .. }, _) => 4,
        (Action::Forget, _) => 6,
    }
}

/// Run the required items and those whose id is in `selected`: containers (`rm -f`), networks,
/// volumes, images (`rm -f`), agent logs to the Trash, then the folder to the Trash, then `forget`.
/// When any earlier step failed, the folder and `forget` are skipped, so a retry finds the same project.
/// `trash` moves paths to the Trash: [`system_trash`] in the app, a fake in tests.
pub fn execute(
    items: &[Item],
    selected: &BTreeSet<String>,
    docker: Option<&dyn Docker>,
    trash: &dyn Fn(&[&PathBuf]) -> Result<(), String>,
    forget: &mut dyn FnMut() -> Result<(), String>,
    on_step: &mut dyn FnMut(&StepResult),
) -> Vec<StepResult> {
    let mut todo: Vec<&Item> = items
        .iter()
        .filter(|i| i.required || selected.contains(&i.id))
        .collect();
    todo.sort_by_key(|i| order(&i.action, i.group));
    let mut results = Vec::new();
    let mut failed = false;
    for item in todo {
        let docker_run = |args: &[&str]| -> Result<(), String> {
            let d = docker.ok_or("Docker is not available")?;
            d.run(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .map(|_| ())
        };
        let outcome = match &item.action {
            _ if failed && matches!(item.group, Group::Folder | Group::Logpose) => {
                Err("skipped because an earlier step failed".to_string())
            }
            Action::Container { id } => docker_run(&["rm", "-f", id]),
            Action::Network { id } => docker_run(&["network", "rm", id]),
            Action::Volume { name } => docker_run(&["volume", "rm", name]),
            Action::Image { id } => docker_run(&["image", "rm", "-f", id]),
            Action::Trash { paths } => {
                let existing: Vec<&PathBuf> = paths.iter().filter(|p| p.exists()).collect();
                if existing.is_empty() {
                    Ok(())
                } else {
                    trash(&existing)
                }
            }
            Action::Forget => forget(),
        };
        let r = StepResult {
            id: item.id.clone(),
            ok: outcome.is_ok(),
            bytes: if outcome.is_ok() { item.bytes } else { 0 },
            trashed: matches!(item.action, Action::Trash { .. }),
            error: outcome.err(),
        };
        failed |= !r.ok;
        on_step(&r);
        results.push(r);
    }
    results
}

/// Total disk use Docker reports (images, containers, volumes, build cache; `docker system df`).
/// Measured before and after a removal: images share layers, so the real amount freed is the
/// difference, not the sum of the per-image sizes.
pub fn docker_usage(d: &dyn Docker) -> Option<u64> {
    let out = d
        .run(&[
            "system".to_string(),
            "df".into(),
            "--format".into(),
            "json".into(),
        ])
        .ok()?;
    Some(
        out.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| parse_docker_size(&s(&v, "Size")))
            .sum(),
    )
}

/// What a removal did: each step, and the Docker space actually freed (`None` without Docker).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outcome {
    pub steps: Vec<StepResult>,
    pub docker_freed: Option<u64>,
}

/// The desktop Trash (freedesktop.org Trash on Linux, Finder's Trash, the Recycle Bin).
pub fn system_trash(paths: &[&PathBuf]) -> Result<(), String> {
    trash::delete_all(paths).map_err(|e| e.to_string())
}

/// `git ls-remote --heads <url>` without prompting for credentials, killed after `timeout`.
/// Returns the number of branches on the remote.
pub fn check_remote(url: &str, timeout: Duration) -> Result<usize, String> {
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--heads", "--", url])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=8");
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            return if status.success() {
                Ok(String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .count())
            } else {
                Err(String::from_utf8_lossy(&out.stderr)
                    .trim()
                    .lines()
                    .last()
                    .unwrap_or("git ls-remote failed")
                    .to_string())
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no answer within {} s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn touch(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn docker_sizes() {
        assert_eq!(parse_docker_size("14.9GB"), 14_900_000_000);
        assert_eq!(parse_docker_size("108kB"), 108_000);
        assert_eq!(parse_docker_size("0B"), 0);
        assert_eq!(parse_docker_size("1.5MiB"), 1_572_864);
        assert_eq!(parse_docker_size("N/A"), 0);
    }

    #[test]
    fn compose_project_names() {
        assert_eq!(normalize_compose_name("My Project.v2"), "myprojectv2");
        assert_eq!(normalize_compose_name("_x-Y"), "x-y");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Shop");
        std::fs::create_dir_all(p.join("deploy/compose")).unwrap();
        std::fs::write(p.join("docker-compose.yml"), "services:\n  web:\n    image: x\n").unwrap();
        std::fs::write(
            p.join("deploy/compose/compose.yaml"),
            "name: \"shop-prod\" # pinned\nservices: {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(p.join("ops")).unwrap();
        std::fs::write(p.join("ops/compose.yml"), "services: {}\n").unwrap();
        std::fs::write(p.join("ops/.env"), "A=1\nCOMPOSE_PROJECT_NAME=shopops\n").unwrap();
        std::fs::create_dir_all(p.join("node_modules/pkg")).unwrap();
        std::fs::write(p.join("node_modules/pkg/compose.yml"), "").unwrap();
        assert_eq!(
            compose_names(&p),
            BTreeSet::from(["shop".into(), "shop-prod".into(), "shopops".into()])
        );
    }

    fn inv() -> Inventory {
        let c = |id: &str, wd: Option<&str>, project: Option<&str>, image: &str, vols: &[&str]| DContainer {
            id: id.into(),
            name: format!("{id}-1"),
            image_id: image.into(),
            image: format!("img-{image}"),
            state: "exited".into(),
            working_dir: wd.map(PathBuf::from),
            project: project.map(str::to_string),
            volumes: vols.iter().map(|v| v.to_string()).collect(),
            size: 100,
        };
        let img = |id: &str, project: Option<&str>| DImage {
            id: id.into(),
            tags: vec![format!("{id}:latest")],
            project: project.map(str::to_string),
            unique: 1000,
        };
        let vol = |name: &str, project: Option<&str>| DVolume {
            name: name.into(),
            project: project.map(str::to_string),
            size: 10,
        };
        Inventory {
            containers: vec![
                c(
                    "web",
                    Some("/p/shop"),
                    Some("shop"),
                    "built",
                    &["anon1", "shop_db"],
                ),
                c("worker", Some("/p/shop/deploy"), Some("shop"), "pulled", &[]),
                c(
                    "other",
                    Some("/p/shopping"),
                    Some("shopping"),
                    "shared",
                    &["shared_vol"],
                ),
            ],
            images: vec![
                img("built", Some("shop")),
                img("pulled", None),
                img("old-build", Some("shop")),
                img("guess", Some("shopops")),
                img("shared", Some("shop")),
                img("unrelated", Some("zzz")),
            ],
            volumes: vec![
                vol("anon1", None),
                vol("shop_db", Some("shop")),
                vol("shop_cache", Some("shop")),
                vol("shopops_data", Some("shopops")),
                vol("shared_vol", Some("shop")),
                vol("zzz_data", Some("zzz")),
            ],
            networks: vec![
                DNetwork {
                    id: "n1".into(),
                    name: "shop_default".into(),
                    project: Some("shop".into()),
                },
                DNetwork {
                    id: "n2".into(),
                    name: "shopping_default".into(),
                    project: Some("shopping".into()),
                },
            ],
        }
    }

    #[test]
    fn docker_matching_rules() {
        let names = BTreeSet::from(["shop".to_string(), "shopops".to_string()]);
        let items = docker_items(&inv(), Path::new("/p/shop"), &names);
        let get = |id: &str| items.iter().find(|i| i.id == id);
        // Containers by working dir (a sibling `/p/shopping` is not inside `/p/shop`), plus their network.
        assert!(get("container:web").is_some() && get("container:worker").is_some());
        assert!(get("container:other").is_none());
        assert!(get("network:n1").is_some_and(|i| i.selected) && get("network:n2").is_none());
        // Volumes: mounted or confirmed by a container → certain and ticked; name only → probable.
        for v in ["volume:anon1", "volume:shop_db", "volume:shop_cache"] {
            let i = get(v).unwrap();
            assert!(
                i.certainty == Certainty::Certain && i.selected && i.checks.is_empty(),
                "{v}"
            );
        }
        let probable = get("volume:shopops_data").unwrap();
        assert!(probable.certainty == Certainty::Probable && !probable.selected);
        assert_eq!(probable.checks[0], "docker volume inspect shopops_data");
        // Used by another project's container → never offered.
        assert!(get("volume:shared_vol").is_none() && get("image:shared").is_none());
        assert!(get("volume:zzz_data").is_none() && get("image:unrelated").is_none());
        // Images: used by my containers (built or pulled), built for my project, or probable.
        assert!(
            get("image:built")
                .unwrap()
                .detail
                .as_deref()
                .unwrap()
                .starts_with("built for this project")
        );
        assert!(
            get("image:pulled")
                .unwrap()
                .detail
                .as_deref()
                .unwrap()
                .starts_with("pulled")
        );
        assert!(get("image:old-build").is_some_and(|i| i.selected && i.certainty == Certainty::Certain));
        let guess = get("image:guess").unwrap();
        assert!(!guess.selected && guess.checks.len() == 2);
        // Without containers, "shop" is only a guess from the compose files.
        let items = docker_items(&inv(), Path::new("/p/elsewhere"), &names);
        assert!(
            items
                .iter()
                .all(|i| i.certainty == Certainty::Probable && !i.selected)
        );
        assert!(items.iter().any(|i| i.id == "volume:shop_cache"));
    }

    struct FakeDocker {
        calls: Mutex<Vec<String>>,
        fail: Option<&'static str>,
    }

    impl Docker for FakeDocker {
        fn run(&self, args: &[String]) -> Result<String, String> {
            let line = args.join(" ");
            self.calls.lock().unwrap().push(line.clone());
            if self.fail.is_some_and(|f| line.starts_with(f)) {
                return Err(format!("failed: {line}"));
            }
            Ok(match args.first().map(String::as_str).unwrap_or_default() {
                "system" => r#"{"Images":[{"ID":"sha256:aa","UniqueSize":"1.5GB"}],"Volumes":[{"Name":"v1","Size":"20MB"}],"Containers":[{"ID":"c1","Size":"108kB"}],"BuildCache":[]}"#.into(),
                "ps" => "c1\n".into(),
                "container" => r#"[{"Id":"c1","Name":"/web-1","Image":"sha256:aa","Config":{"Image":"web:latest","Labels":{"com.docker.compose.project":"shop","com.docker.compose.project.working_dir":"/p/shop"}},"State":{"Status":"running"},"Mounts":[{"Type":"bind","Source":"/p/shop/data"},{"Type":"volume","Name":"v1"}]}]"#.into(),
                "image" if args.get(1).is_some_and(|a| a == "ls") => "sha256:aa\nsha256:aa\n".into(),
                "image" => r#"[{"Id":"sha256:aa","RepoTags":["web:latest"],"Config":{"Labels":{"com.docker.compose.project":"shop"}}}]"#.into(),
                "volume" if args.get(1).is_some_and(|a| a == "ls") => "v1\n".into(),
                "volume" => r#"[{"Name":"v1","Labels":null}]"#.into(),
                "network" if args.get(1).is_some_and(|a| a == "ls") => "".into(),
                _ => String::new(),
            })
        }
    }

    #[test]
    fn inventory_from_docker_json() {
        let d = FakeDocker {
            calls: Mutex::new(Vec::new()),
            fail: None,
        };
        let inv = inventory(&d).unwrap();
        assert_eq!(
            inv.containers,
            [DContainer {
                id: "c1".into(),
                name: "web-1".into(),
                image_id: "sha256:aa".into(),
                image: "web:latest".into(),
                state: "running".into(),
                working_dir: Some("/p/shop".into()),
                project: Some("shop".into()),
                volumes: vec!["v1".into()],
                size: 108_000,
            }]
        );
        assert_eq!(inv.images[0].unique, 1_500_000_000);
        assert_eq!(
            (
                inv.images.len(),
                inv.volumes[0].size,
                inv.volumes[0].project.clone()
            ),
            (1, 20_000_000, None)
        );
        assert!(inv.networks.is_empty());
        let calls = d.calls.lock().unwrap();
        assert!(
            calls.contains(&"image inspect sha256:aa".to_string()),
            "ids deduplicated: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.starts_with("network inspect")),
            "no inspect for an empty list"
        );
    }

    fn session(agent: Agent, cwd: &str, files: Vec<PathBuf>) -> Session {
        Session {
            agent,
            id: cwd.into(),
            cwd: cwd.into(),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            prompts: vec![],
            final_message: None,
            title: None,
            git_branch: None,
            tokens: Default::default(),
            files,
        }
    }

    #[test]
    fn agent_log_items() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude/projects");
        let whole = claude.join("-p-shop");
        touch(&whole.join("s1.jsonl"), 100);
        touch(&whole.join("s1/tool-results/r.txt"), 50);
        touch(&whole.join("s2.jsonl"), 100);
        let mixed = claude.join("-p");
        touch(&mixed.join("s3.jsonl"), 100);
        touch(&mixed.join("elsewhere.jsonl"), 100);
        let codex = dir.path().join("codex/r1.jsonl");
        touch(&codex, 300);
        let sessions = vec![
            session(Agent::ClaudeCode, "/p/shop", vec![whole.join("s1.jsonl")]),
            session(Agent::ClaudeCode, "/p/shop/app", vec![whole.join("s2.jsonl")]),
            session(Agent::ClaudeCode, "/p/shop", vec![mixed.join("s3.jsonl")]),
            session(
                Agent::ClaudeCode,
                "/p/shopping",
                vec![mixed.join("elsewhere.jsonl")],
            ),
            session(Agent::Codex, "/p/shop", vec![codex.clone()]),
        ];
        let items = log_items(Path::new("/p/shop"), &sessions);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| !i.selected && i.group == Group::AgentLogs));
        assert_eq!(items[0].label, "Claude Code: 3 sessions (2 files)");
        assert_eq!(
            items[0].action,
            Action::Trash {
                paths: vec![mixed.join("s3.jsonl"), whole.clone()]
            },
            "whole folder when every entry is this project's; single files otherwise"
        );
        assert!(items[0].bytes >= 350);
        assert_eq!(items[1].action, Action::Trash { paths: vec![codex] });
        assert!(log_items(Path::new("/q"), &sessions).is_empty());
    }

    #[test]
    fn folder_size_with_build_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        touch(&p.join("src/main.rs"), 10_000);
        touch(&p.join("target/debug/big"), 200_000);
        touch(&p.join("target/debug/node_modules/x"), 10_000);
        touch(&p.join("app/node_modules/pkg/index.js"), 50_000);
        let (total, outs) = folder_size(p);
        assert!(total >= 270_000, "{total}");
        let names: Vec<_> = outs
            .iter()
            .map(|o| o.path.strip_prefix(p).unwrap().to_path_buf())
            .collect();
        assert_eq!(
            names,
            [PathBuf::from("target"), PathBuf::from("app/node_modules")],
            "nested outputs are not listed twice"
        );
        assert!(outs[0].bytes >= 210_000);
    }

    fn items_for_execute(dir: &Path) -> Vec<Item> {
        let item = |id: &str, group: Group, action: Action, required: bool| Item {
            id: id.into(),
            group,
            label: id.into(),
            detail: None,
            bytes: 10,
            certainty: Certainty::Certain,
            selected: true,
            required,
            checks: vec![],
            action,
        };
        vec![
            item(
                "folder",
                Group::Folder,
                Action::Trash {
                    paths: vec![dir.join("proj")],
                },
                true,
            ),
            item("image:i", Group::Images, Action::Image { id: "i".into() }, false),
            item(
                "logs:codex",
                Group::AgentLogs,
                Action::Trash {
                    paths: vec![dir.join("log.jsonl")],
                },
                false,
            ),
            item(
                "volume:v",
                Group::Volumes,
                Action::Volume { name: "v".into() },
                false,
            ),
            item(
                "container:c",
                Group::Containers,
                Action::Container { id: "c".into() },
                false,
            ),
            item(
                "network:n",
                Group::Containers,
                Action::Network { id: "n".into() },
                false,
            ),
            item("logpose", Group::Logpose, Action::Forget, true),
        ]
    }

    #[test]
    fn execute_order_and_failure_stop() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("proj/f"), 1);
        touch(&dir.path().join("log.jsonl"), 1);
        let items = items_for_execute(dir.path());
        let trashed = Mutex::new(Vec::new());
        let trash = |paths: &[&PathBuf]| {
            trashed
                .lock()
                .unwrap()
                .extend(paths.iter().map(|p| p.to_path_buf()));
            Ok(())
        };

        // Everything ticked except the volume: Docker first, then logs, folder, Logpose.
        let d = FakeDocker {
            calls: Mutex::new(Vec::new()),
            fail: None,
        };
        let selected: BTreeSet<String> = ["image:i", "logs:codex", "container:c", "network:n"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut forgot = 0;
        let mut steps = Vec::new();
        let r = execute(
            &items,
            &selected,
            Some(&d),
            &trash,
            &mut || {
                forgot += 1;
                Ok(())
            },
            &mut |s| steps.push(s.id.clone()),
        );
        assert_eq!(
            steps,
            [
                "container:c",
                "network:n",
                "image:i",
                "logs:codex",
                "folder",
                "logpose"
            ]
        );
        assert!(r.iter().all(|s| s.ok));
        assert_eq!(
            *d.calls.lock().unwrap(),
            ["rm -f c", "network rm n", "image rm -f i"]
        );
        assert_eq!(
            *trashed.lock().unwrap(),
            [dir.path().join("log.jsonl"), dir.path().join("proj")]
        );
        assert_eq!(forgot, 1);
        assert!(r.iter().find(|s| s.id == "folder").unwrap().trashed);

        // A failed Docker step: later Docker steps still run, the folder and Logpose data stay.
        trashed.lock().unwrap().clear();
        let d = FakeDocker {
            calls: Mutex::new(Vec::new()),
            fail: Some("rm -f"),
        };
        let mut forgot = 0;
        let r = execute(
            &items,
            &selected,
            Some(&d),
            &trash,
            &mut || {
                forgot += 1;
                Ok(())
            },
            &mut |_| {},
        );
        let by_id = |id: &str| r.iter().find(|s| s.id == id).unwrap().clone();
        assert!(!by_id("container:c").ok && by_id("image:i").ok);
        assert_eq!(
            by_id("folder").error.as_deref(),
            Some("skipped because an earlier step failed")
        );
        assert_eq!((by_id("folder").bytes, forgot), (0, 0));
        assert_eq!(*trashed.lock().unwrap(), [dir.path().join("log.jsonl")]);

        // No Docker: Docker items fail with a clear message, nothing else is touched.
        let r = execute(&items, &selected, None, &trash, &mut || Ok(()), &mut |_| {});
        assert_eq!(r[0].error.as_deref(), Some("Docker is not available"));
    }

    #[test]
    fn remote_check_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        crate::git::testutil::git(dir.path(), &["init", "-q", "--bare", "-b", "main", "r.git"]);
        assert_eq!(
            check_remote(
                dir.path().join("r.git").to_str().unwrap(),
                Duration::from_secs(10)
            ),
            Ok(0)
        );
        assert!(
            check_remote(
                dir.path().join("missing.git").to_str().unwrap(),
                Duration::from_secs(10)
            )
            .is_err()
        );
    }

    /// Real Docker, opt-in (`LOGPOSE_LIVE_DOCKER=1`): a throwaway compose project made by the test
    /// (`docker compose create`, nothing runs) is planned and removed; images are never removed.
    #[test]
    fn live_docker_removal() {
        if std::env::var_os("LOGPOSE_LIVE_DOCKER").is_none() {
            return;
        }
        let d = DockerCli::find().expect("docker on PATH");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("logpose-e2e");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("compose.yaml"),
            "name: logpose-e2e\nservices:\n  app:\n    image: alpine:3.21\n    command: [\"sleep\", \"1\"]\n    volumes: [data:/data]\nvolumes:\n  data: {}\n",
        )
        .unwrap();
        let up = Command::new("docker")
            .args(["compose", "create"])
            .current_dir(&p)
            .output()
            .unwrap();
        assert!(up.status.success(), "{}", String::from_utf8_lossy(&up.stderr));

        let inv = inventory(&d).unwrap();
        let items = docker_items(&inv, &p, &compose_names(&p));
        let ids: BTreeSet<String> = items
            .iter()
            .filter(|i| i.group != Group::Images)
            .map(|i| i.id.clone())
            .collect();
        let kinds: Vec<(Group, Certainty)> = items.iter().map(|i| (i.group, i.certainty)).collect();
        assert!(
            kinds.contains(&(Group::Containers, Certainty::Certain)),
            "{items:#?}"
        );
        assert!(
            items
                .iter()
                .any(|i| i.id == "volume:logpose-e2e_data" && i.certainty == Certainty::Certain),
            "{items:#?}"
        );
        assert!(
            items.iter().any(|i| i.label == "network logpose-e2e_default"),
            "{items:#?}"
        );

        let r = execute(&items, &ids, Some(&d), &|_| Ok(()), &mut || Ok(()), &mut |_| {});
        assert!(r.iter().all(|s| s.ok), "{r:#?}");
        let after = inventory(&d).unwrap();
        assert!(
            !after
                .containers
                .iter()
                .any(|c| c.project.as_deref() == Some("logpose-e2e"))
        );
        assert!(!after.volumes.iter().any(|v| v.name == "logpose-e2e_data"));
        assert!(!after.networks.iter().any(|n| n.name == "logpose-e2e_default"));
        assert!(
            after
                .images
                .iter()
                .any(|i| i.tags.iter().any(|t| t == "alpine:3.21")),
            "images untouched"
        );
    }
}
