//! Tauri shell around `logpose-core`. Commands stay thin: they load config, call the core, and
//! return serializable results to the Svelte frontend.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use logpose_core::agent_rule;
use logpose_core::config::{self, Config};
use logpose_core::digest;
use logpose_core::git;
use logpose_core::platform::Platform;
use logpose_core::runner;
use logpose_core::scan::{self, LogSources, ScanReport};
use logpose_core::sessions::merge::normalize_path;
use logpose_core::sessions::{Agent, claude, codex};
use logpose_core::stats::{self, BucketCounts, GitStats, MonthMoves, SessionStats};
use logpose_core::store::{self, BoardColumn, ProjectRow, Store};
use logpose_core::{packer, removal, synth, synthesis};
use serde::{Deserialize, Serialize};
use tauri::State;

/// Optional tray mode (LATER-003): flag, and the file watcher while it is on.
#[derive(Default, Clone)]
struct Tray {
    on: Arc<std::sync::atomic::AtomicBool>,
    watch: Arc<Mutex<Option<logpose_core::watch::WatchHandle>>>,
}

const TRAY_KEY: &str = "tray.enabled";
const TRAY_ID: &str = "logpose";

impl Tray {
    fn is_on(&self) -> bool {
        self.on.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// (Re)start watching the projects of `report`; changes emit `files-changed` to the UI.
    fn watch(&self, app: &tauri::AppHandle, report: &ScanReport) {
        use tauri::Emitter;
        let Ok(mut guard) = self.watch.lock() else { return };
        *guard = None;
        if !self.is_on() {
            return;
        }
        let targets = logpose_core::watch::targets(report, &LogSources::defaults());
        let app = app.clone();
        match logpose_core::watch::start(&targets, move || {
            let _ = app.emit("files-changed", ());
        }) {
            Ok(h) => {
                println!("tray: watching {} folders", h.watched);
                *guard = Some(h);
            }
            Err(e) => eprintln!("tray: cannot watch: {e}"),
        }
    }
}

/// Show the tray icon with its menu (idempotent).
fn add_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::Emitter;
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;
    if app.tray_by_id(TRAY_ID).is_some() {
        return Ok(());
    }
    let show = MenuItem::with_id(app, "show", "Show Logpose", true, None::<&str>)?;
    let rescan = MenuItem::with_id(app, "rescan", "Rescan now", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Logpose", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &rescan, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Logpose")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_window(app),
            "rescan" => {
                let _ = app.emit("tray-rescan", ());
            }
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Logical width the UI needs to show the five board columns next to the expanded sidebar
/// (UI-004): sidebar 248 + 1 border, five 13 rem (208 px) columns, four 0.75 rem gaps, 1.5 rem
/// padding on each side. Keep in sync with `Home.svelte` (`boardMin`) and `Board.svelte`.
const IDEAL_WIDTH: f64 = 249.0 + 5.0 * 208.0 + 4.0 * 12.0 + 2.0 * 24.0;
const IDEAL_HEIGHT: f64 = 900.0;

/// Window size for a monitor work area of `avail` logical pixels: the ideal size with some room
/// to spare, never more than 96 % × 92 % of the work area, and never below a usable minimum
/// unless the screen itself is smaller.
fn fit_window_size(avail_w: f64, avail_h: f64) -> (f64, f64) {
    let w = (IDEAL_WIDTH + 40.0)
        .min(avail_w * 0.96)
        .max(900.0_f64.min(avail_w));
    let h = IDEAL_HEIGHT.min(avail_h * 0.92).max(600.0_f64.min(avail_h));
    (w.round(), h.round())
}

/// Size and center the main window for the monitor it opens on (UI-004).
fn fit_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    let Some(m) = w
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| w.primary_monitor().ok().flatten())
    else {
        return;
    };
    let scale = m.scale_factor();
    let area = m.work_area().size;
    let (width, height) = fit_window_size(f64::from(area.width) / scale, f64::from(area.height) / scale);
    let _ = w.set_size(tauri::LogicalSize::new(width, height));
    let _ = w.center();
    println!(
        "window: {width}x{height} for a {}x{} work area (scale {scale})",
        f64::from(area.width) / scale,
        f64::from(area.height) / scale
    );
}

fn show_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[tauri::command]
fn tray_enabled(tray: State<'_, Tray>) -> bool {
    tray.is_on()
}

/// Turn tray mode on (icon, close-to-tray, watching) or off (Quit on close, no watching).
#[tauri::command]
fn set_tray_enabled(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    tray: State<'_, Tray>,
    last: State<'_, LastScan>,
    enabled: bool,
) -> Result<bool, String> {
    db.with(|s| s.set_setting(TRAY_KEY, if enabled { "true" } else { "false" }))?;
    tray.on.store(enabled, std::sync::atomic::Ordering::SeqCst);
    if enabled {
        add_tray(&app).map_err(|e| e.to_string())?;
        if let Some(report) = last.0.lock().ok().and_then(|g| g.clone()) {
            tray.watch(&app, &report);
        }
    } else {
        let _ = app.remove_tray_by_id(TRAY_ID);
        if let Ok(mut w) = tray.watch.lock() {
            *w = None;
        }
    }
    Ok(enabled)
}

/// The last scan, kept for synthesis (the core needs the full `ProjectReport`s).
#[derive(Default, Clone)]
struct LastScan(Arc<Mutex<Option<ScanReport>>>);

/// The SQLite store, opened on first use (board state, snapshots, notes).
#[derive(Default, Clone)]
struct Db(Arc<Mutex<Option<Store>>>);

impl Db {
    fn with<T>(&self, f: impl FnOnce(&mut Store) -> store::Result<T>) -> Result<T, String> {
        let mut guard = self.0.lock().map_err(|_| "store lock poisoned".to_string())?;
        if guard.is_none() {
            let path = store::default_path().ok_or("cannot determine the data directory")?;
            *guard = Some(Store::open(&path).map_err(|e| format!("{}: {e}", path.display()))?);
        }
        f(guard.as_mut().expect("opened above")).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Serialize)]
struct SetupState {
    config_path: String,
    /// Roots exactly as configured (`~` expanded).
    roots: Vec<String>,
    /// True until at least one root is configured: the UI blocks on the setup screen (ONBOARD-001).
    needs_setup: bool,
}

fn config_path() -> Result<PathBuf, String> {
    config::default_path().ok_or_else(|| "cannot determine the config path".to_string())
}

fn setup_state(path: PathBuf, config: &Config) -> SetupState {
    SetupState {
        config_path: path.display().to_string(),
        roots: config.roots.iter().map(|r| r.display().to_string()).collect(),
        needs_setup: config.roots.is_empty(),
    }
}

#[tauri::command]
fn get_setup() -> Result<SetupState, String> {
    let path = config_path()?;
    let config = Config::load(&path).map_err(|e| e.to_string())?;
    Ok(setup_state(path, &config))
}

/// Save the chosen roots (ONBOARD-001 / SET-001). Every root must be an existing directory.
#[tauri::command]
fn save_roots(roots: Vec<String>) -> Result<SetupState, String> {
    let roots: Vec<PathBuf> = roots
        .iter()
        .map(|r| PathBuf::from(r.trim()))
        .filter(|r| !r.as_os_str().is_empty())
        .collect();
    if roots.is_empty() {
        return Err("add at least one folder".into());
    }
    if let Some(bad) = roots.iter().find(|r| !r.is_dir()) {
        return Err(format!("not a folder: {}", bad.display()));
    }
    let path = config_path()?;
    config::save_roots(&path, &roots, Platform::current().home().as_deref()).map_err(|e| e.to_string())?;
    let config = Config::load(&path).map_err(|e| e.to_string())?;
    Ok(setup_state(path, &config))
}

#[derive(Debug, Serialize)]
struct Settings {
    config_path: String,
    /// `~` expanded.
    locations: config::Locations,
}

/// Roots, exclusions and aliases as configured (SET-001).
#[tauri::command]
fn get_settings() -> Result<Settings, String> {
    let path = config_path()?;
    let config = Config::load(&path).map_err(|e| e.to_string())?;
    Ok(Settings {
        config_path: path.display().to_string(),
        locations: config::Locations {
            roots: config.roots,
            exclude: config.exclude,
            aliases: config.aliases,
        },
    })
}

/// Save roots, exclusions and aliases, keeping the rest of `config.toml` (SET-001). Roots must be
/// existing folders; empty exclusions and aliases are dropped.
#[tauri::command]
fn save_settings(mut locations: config::Locations) -> Result<Settings, String> {
    locations.roots = locations
        .roots
        .iter()
        .map(|r| PathBuf::from(r.to_string_lossy().trim()))
        .filter(|r| !r.as_os_str().is_empty())
        .collect();
    if locations.roots.is_empty() {
        return Err("add at least one folder".into());
    }
    if let Some(bad) = locations.roots.iter().find(|r| !r.is_dir()) {
        return Err(format!("not a folder: {}", bad.display()));
    }
    locations.exclude.retain(|g| !g.trim().is_empty());
    let path = config_path()?;
    config::save_locations(&path, &locations, Platform::current().home().as_deref())
        .map_err(|e| e.to_string())?;
    get_settings()
}

/// Zero-token scan of the configured roots, persisted to the store (new projects land in Next).
/// Runs off the main thread.
#[tauri::command]
async fn scan(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    last: State<'_, LastScan>,
    tray: State<'_, Tray>,
) -> Result<ScanReport, String> {
    let db = db.inner().clone();
    let last = last.inner().clone();
    let tray = tray.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let path = config_path()?;
        let config = Config::load(&path).map_err(|e| e.to_string())?;
        if config.roots.is_empty() {
            return Err(format!("no roots configured in {}", path.display()));
        }
        let report = scan::scan(&config, &LogSources::defaults(), Utc::now())
            .map_err(|e| format!("invalid exclude glob: {e}"))?;
        println!(
            "scan: {} projects in {:.2?}",
            report.projects.len(),
            report.duration
        );
        // First launch (empty board): place projects by the suggested heuristic (BOARD-003).
        db.with(|s| {
            let first = s.projects()?.is_empty();
            scan::persist(s, &report)?;
            if first {
                logpose_core::board::apply_suggested(s, &report.projects, Utc::now())?;
            }
            // DEL-004: drop data nothing uses any more. A project is forgotten only when its folder
            // is gone but the folder containing it is still there (an unmounted disk keeps its cards).
            let gone = |p: &std::path::Path| !p.exists() && p.parent().is_some_and(|d| d.is_dir());
            let pruned = s.prune(Utc::now(), &chrono::Local, &gone)?;
            if pruned != store::PruneReport::default() {
                println!(
                    "prune: {} projects, {} cached summaries, {} snapshots",
                    pruned.projects, pruned.summaries, pruned.snapshots
                );
            }
            Ok(())
        })?;
        if let Ok(mut guard) = last.0.lock() {
            *guard = Some(report.clone());
        }
        tray.watch(&app, &report);
        Ok(report)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Serialize)]
struct CommitLine {
    at: chrono::DateTime<Utc>,
    subject: String,
}

/// Last first-parent commits of a repository for the detail view (DETAIL-001).
#[tauri::command]
async fn recent_commits(path: String, limit: usize) -> Result<Vec<CommitLine>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        git::recent_commits(std::path::Path::new(&path), limit.min(50))
            .map(|v| {
                v.into_iter()
                    .map(|(at, subject)| CommitLine { at, subject })
                    .collect()
            })
            .map_err(|e| e.message)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Deserialize)]
struct StatsProject {
    path: PathBuf,
    /// The project's own repo and sub-repos (those the scan could read).
    repos: Vec<PathBuf>,
}

#[derive(Debug, Serialize)]
struct StatsView {
    git: GitStats,
    sessions: SessionStats,
    buckets: std::collections::BTreeMap<chrono::NaiveDate, BucketCounts>,
    moves: std::collections::BTreeMap<chrono::NaiveDate, MonthMoves>,
}

/// Retroactive stats for the projects of the last scan (STATS-001..003) from local day `from` through
/// local day `to` (inclusive; `to` defaults to today), bucketed in the local time zone.
#[tauri::command]
async fn stats(
    db: State<'_, Db>,
    projects: Vec<StatsProject>,
    from: chrono::NaiveDate,
    to: Option<chrono::NaiveDate>,
) -> Result<StatsView, String> {
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let started = std::time::Instant::now();
        let local = chrono::Local;
        let midnight = |d: chrono::NaiveDate| {
            d.and_hms_opt(0, 0, 0)
                .and_then(|t| t.and_local_timezone(local).earliest())
                .map(|t| t.with_timezone(&Utc))
                .ok_or_else(|| format!("invalid date {d}"))
        };
        let since = midnight(from)?;
        let until = to.and_then(|d| d.succ_opt()).map(midnight).transpose()?;
        if until.is_some_and(|u| u <= since) {
            return Err("the end date is before the start date".into());
        }
        let pairs: Vec<(PathBuf, Vec<PathBuf>)> = projects.into_iter().map(|p| (p.path, p.repos)).collect();
        let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
        let git = stats::git_stats_for(
            &pairs,
            stats::Authors::RepoEmail(&config.stats.emails),
            since,
            until,
            &local,
        );

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
        sessions.retain(|s| s.started_at >= since && until.is_none_or(|u| s.started_at < u));
        let paths: Vec<PathBuf> = pairs.iter().map(|(p, _)| p.clone()).collect();
        let sessions = stats::session_stats(&sessions, &paths, &local);

        let (mut snapshots, rows) = db.with(|s| Ok((s.snapshots_since(since)?, s.projects()?)))?;
        if let Some(u) = until {
            snapshots.retain(|s| s.taken_at < u);
        }
        let view = StatsView {
            git,
            sessions,
            buckets: stats::bucket_series(&snapshots, &local),
            moves: stats::moves_per_month(&rows, &local),
        };
        println!(
            "stats: {from}..{} in {:.2?}",
            to.map_or("today".into(), |d| d.to_string()),
            started.elapsed()
        );
        Ok(view)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Run a tiny synthetic pack through one agent (`claude-code` or `codex`) and report the result or
/// the exact failure (SYN-011). Spends a few tokens.
#[tauri::command]
async fn check_agent(db: State<'_, Db>, agent: String) -> Result<runner::AgentCheck, String> {
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
        let not_found = |name: &str, detail: String| runner::AgentCheck {
            agent: name.into(),
            program: None,
            ok: false,
            model: None,
            tokens: 0,
            millis: 0,
            error: Some(detail),
        };
        Ok(match agent.as_str() {
            "claude-code" => match claude_runner(&config) {
                Ok(c) => {
                    let program = c.program.clone();
                    let check = runner::check_agent(&c, Some(&program));
                    db.with(|s| {
                        remember_claude_limits(s, &c);
                        Ok(())
                    })?;
                    check
                }
                Err(e) => not_found("claude-code", e),
            },
            "codex" => match runner::CodexRunner::discover(
                Some(&config.agent.program_codex),
                Some(config.agent.model_codex.clone()),
            ) {
                Ok(r) => {
                    let program = r.program.clone();
                    runner::check_agent(&r, Some(&program))
                }
                Err(e) => not_found("codex", e.to_string()),
            },
            other => return Err(format!("unknown agent: {other}")),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn claude_runner(config: &Config) -> Result<runner::ClaudeRunner, String> {
    let configured = &config.agent.program_claude;
    let program = logpose_core::launcher::find_agent(Agent::ClaudeCode, configured)
        .ok_or_else(|| logpose_core::launcher::agent_not_found(Agent::ClaudeCode, configured))?;
    Ok(runner::ClaudeRunner::new(
        program,
        config.agent.model_claude.clone(),
    ))
}

/// Keep the plan usage a Claude call reported, for the picker (SYN-009).
fn remember_claude_limits(store: &Store, claude: &runner::ClaudeRunner) {
    if let Some(l) = claude.limits() {
        let _ = logpose_core::usage::save_claude_limits(store, &l);
    }
}

/// A project directory that lies inside a configured root (guards commands that write files).
fn project_dir(path: &str) -> Result<PathBuf, String> {
    let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
    let dir = PathBuf::from(path);
    if !dir.is_dir() || !config.roots.iter().any(|r| dir.starts_with(r)) {
        return Err(format!("not a project folder inside your roots: {path}"));
    }
    Ok(dir)
}

/// The last removal plan, reused by `remove_project` so the folder is not measured twice.
#[derive(Default, Clone)]
struct LastPlan(Arc<Mutex<Option<(std::time::Instant, removal::Plan)>>>);

/// A project of the last scan that may be removed: inside a root, and not a root itself.
fn removable(last: &LastScan, path: &str) -> Result<logpose_core::scan::ProjectReport, String> {
    let dir = project_dir(path)?;
    let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
    if config
        .roots
        .iter()
        .any(|r| normalize_path(r) == normalize_path(&dir))
    {
        return Err("a project folder setting cannot be removed".into());
    }
    last.0
        .lock()
        .map_err(|_| "scan lock poisoned".to_string())?
        .as_ref()
        .and_then(|r| r.projects.iter().find(|p| p.project.path == dir).cloned())
        .ok_or_else(|| "rescan first: this project is not in the last scan".into())
}

fn all_sessions() -> Vec<logpose_core::sessions::Session> {
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
    sessions
}

/// Everything removing a project would touch, with sizes (DEL-001). Changes nothing.
#[tauri::command]
async fn removal_plan(
    last: State<'_, LastScan>,
    plans: State<'_, LastPlan>,
    path: String,
) -> Result<removal::Plan, String> {
    let report = removable(last.inner(), &path)?;
    let plans = plans.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let docker = removal::DockerCli::find();
        let plan = removal::plan(
            &report,
            &all_sessions(),
            docker.as_ref().map(|d| d as &dyn removal::Docker),
        );
        println!(
            "removal plan: {} items for {} in {} ms",
            plan.items.len(),
            path,
            plan.millis
        );
        if let Ok(mut g) = plans.0.lock() {
            *g = Some((std::time::Instant::now(), plan.clone()));
        }
        Ok(plan)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether a remote answers `git ls-remote` within 15 s; the number of branches it has.
#[tauri::command]
async fn check_remote(url: String) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        removal::check_remote(&url, std::time::Duration::from_secs(15))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove a project (DEL-002): the plan's required items plus the `selected` ones, reported step
/// by step on `on_step`. Uses the plan just shown when it is less than 10 minutes old.
#[tauri::command]
async fn remove_project(
    db: State<'_, Db>,
    last: State<'_, LastScan>,
    plans: State<'_, LastPlan>,
    path: String,
    selected: Vec<String>,
    on_step: tauri::ipc::Channel<removal::StepResult>,
) -> Result<removal::Outcome, String> {
    let report = removable(last.inner(), &path)?;
    let (db, plans) = (db.inner().clone(), plans.inner().clone());
    tauri::async_runtime::spawn_blocking(move || {
        let docker = removal::DockerCli::find();
        let docker = docker.as_ref().map(|d| d as &dyn removal::Docker);
        let cached = plans.0.lock().ok().and_then(|g| {
            g.as_ref()
                .filter(|(at, p)| p.project == report.project.path && at.elapsed().as_secs() < 600)
                .map(|(_, p)| p.clone())
        });
        let plan = cached.unwrap_or_else(|| removal::plan(&report, &all_sessions(), docker));
        let selected: std::collections::BTreeSet<String> = selected.into_iter().collect();
        let dir = report.project.path.clone();
        let mut forget = || -> Result<(), String> {
            db.with(|s| s.forget_project(&dir))?;
            // The display name in config.toml, if any.
            let cfg_path = config_path()?;
            let config = Config::load(&cfg_path).map_err(|e| e.to_string())?;
            if config.aliases.contains_key(&dir) {
                let mut aliases = config.aliases.clone();
                aliases.remove(&dir);
                let locations = config::Locations {
                    roots: config.roots,
                    exclude: config.exclude,
                    aliases,
                };
                config::save_locations(&cfg_path, &locations, Platform::current().home().as_deref())
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        };
        let before = docker.and_then(removal::docker_usage);
        let steps = removal::execute(
            &plan.items,
            &selected,
            docker,
            &removal::system_trash,
            &mut forget,
            &mut |r| {
                println!(
                    "remove: {} {}",
                    r.id,
                    if r.ok {
                        "ok".to_string()
                    } else {
                        format!("failed: {}", r.error.as_deref().unwrap_or(""))
                    }
                );
                let _ = on_step.send(r.clone());
            },
        );
        if let Ok(mut g) = plans.0.lock() {
            *g = None;
        }
        let after = docker.and_then(removal::docker_usage);
        let docker_freed = before.zip(after).map(|(b, a)| b.saturating_sub(a));
        Ok(removal::Outcome { steps, docker_freed })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Which instruction files carry the "keep NEXT.md current" rule (LATER-001).
#[tauri::command]
fn agent_rule_status(path: String) -> Result<agent_rule::RuleStatus, String> {
    Ok(agent_rule::status(&project_dir(&path)?))
}

/// Add the rule (to existing CLAUDE.md/AGENTS.md and the files of `agents`) or remove it.
#[tauri::command]
fn set_agent_rule(
    path: String,
    enabled: bool,
    agents: Vec<String>,
) -> Result<agent_rule::RuleStatus, String> {
    let dir = project_dir(&path)?;
    let agents: Vec<Agent> = agents
        .iter()
        .filter_map(|a| match a.as_str() {
            "claude-code" => Some(Agent::ClaudeCode),
            "codex" => Some(Agent::Codex),
            _ => None,
        })
        .collect();
    let result = if enabled {
        agent_rule::enable(&dir, &agents)
    } else {
        agent_rule::disable(&dir)
    };
    result.map_err(|e| e.to_string())?;
    Ok(agent_rule::status(&dir))
}

/// Show the weekly digest as a system notification when it is enabled and due (LATER-004). Called
/// after a scan; returns the listed projects (empty when nothing was shown).
#[tauri::command]
fn digest_if_due(app: tauri::AppHandle, db: State<'_, Db>) -> Result<Vec<digest::Neglected>, String> {
    use tauri_plugin_notification::NotificationExt;
    let Some(items) = db.with(|s| digest::take_if_due(s, Utc::now()))? else {
        return Ok(Vec::new());
    };
    app.notification()
        .builder()
        .title("Logpose: projects waiting for you")
        .body(digest::body(&items))
        .show()
        .map_err(|e| e.to_string())?;
    Ok(items)
}

#[tauri::command]
fn digest_enabled(db: State<'_, Db>) -> Result<bool, String> {
    db.with(|s| digest::enabled(s))
}

#[tauri::command]
fn set_digest_enabled(db: State<'_, Db>, enabled: bool) -> Result<bool, String> {
    db.with(|s| {
        digest::set_enabled(s, enabled)?;
        digest::enabled(s)
    })
}

/// Build synthesis jobs for the projects of the last scan (all, or only `paths`).
fn jobs_for<'a>(
    store: &Store,
    report: &'a ScanReport,
    paths: Option<&[String]>,
    budget: usize,
    force: bool,
) -> store::Result<Vec<synth::Job<'a>>> {
    let rows = store.projects()?;
    Ok(report
        .projects
        .iter()
        .filter(|p| paths.is_none_or(|ps| ps.iter().any(|x| p.project.path == Path::new(x))))
        .filter_map(|p| {
            let row = rows.iter().find(|r| r.path == p.project.path)?;
            Some(synth::Job {
                report: p,
                project_id: row.id,
                options: packer::PackOptions::for_project(row, budget),
                force,
            })
        })
        .collect())
}

#[derive(Debug, Serialize)]
struct KnownEntry {
    path: String,
    known: synth::Known,
}

/// Syntheses available without calling an agent: current `NEXT.md`, cache, or outdated cache.
#[tauri::command]
fn known_syntheses(db: State<'_, Db>, last: State<'_, LastScan>) -> Result<Vec<KnownEntry>, String> {
    let guard = last.0.lock().map_err(|_| "scan lock poisoned".to_string())?;
    let Some(report) = guard.as_ref() else {
        return Ok(Vec::new());
    };
    let budget = Config::load(&config_path()?)
        .map_err(|e| e.to_string())?
        .agent
        .context_budget_tokens;
    db.with(|s| {
        let jobs = jobs_for(s, report, None, budget, false)?;
        Ok(jobs
            .iter()
            .filter_map(|j| {
                synth::known(s, j).map(|known| KnownEntry {
                    path: j.report.project.path.display().to_string(),
                    known,
                })
            })
            .collect())
    })
}

/// One streamed result (event `synthesis`).
#[derive(Debug, Clone, Serialize)]
struct SynthesisUpdate {
    run: String,
    path: String,
    ok: bool,
    synthesis: Option<synthesis::Synthesis>,
    source: Option<synth::Source>,
    error: Option<String>,
}

/// Synthesize the projects of the last scan (all, or `paths`) with `agent`, streaming each result as
/// a `synthesis` event tagged with `run`. `force` skips the cache and `NEXT.md` (Refresh). Uses its
/// own store connection so the board stays usable during the run.
#[tauri::command]
async fn synthesize(
    app: tauri::AppHandle,
    last: State<'_, LastScan>,
    run: String,
    agent: String,
    paths: Option<Vec<String>>,
    force: bool,
) -> Result<synth::SynthesisSummary, String> {
    use tauri::Emitter;
    let report = last
        .0
        .lock()
        .map_err(|_| "scan lock poisoned".to_string())?
        .clone()
        .ok_or("scan first")?;
    tauri::async_runtime::spawn_blocking(move || {
        let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
        let (claude, codex) = match agent.as_str() {
            "claude-code" => (Some(claude_runner(&config)?), None),
            "codex" => (
                None,
                Some(
                    runner::CodexRunner::discover(
                        Some(&config.agent.program_codex),
                        Some(config.agent.model_codex.clone()),
                    )
                    .map_err(|e| e.to_string())?,
                ),
            ),
            other => return Err(format!("unknown agent: {other}")),
        };
        let runner: &dyn runner::Runner = match (&claude, &codex) {
            (Some(c), _) => c,
            (_, Some(x)) => x,
            _ => unreachable!("one runner is set above"),
        };
        let db_path = store::default_path().ok_or("cannot determine the data directory")?;
        let store = Store::open(&db_path).map_err(|e| e.to_string())?;
        let jobs = jobs_for(
            &store,
            &report,
            paths.as_deref(),
            config.agent.context_budget_tokens,
            force,
        )
        .map_err(|e| e.to_string())?;
        let summary = synth::synthesize_batched(
            &store,
            &jobs,
            runner,
            config.agent.concurrency,
            synth::Batching::RECOMMENDED,
            Utc::now(),
            |e| {
                let path = jobs[e.index].report.project.path.display().to_string();
                let (ok, synthesis, source, error) = match e.result {
                    Ok((s, src)) => (true, Some(s), Some(src), None),
                    Err(err) => (false, None, None, Some(err.to_string())),
                };
                let _ = app.emit(
                    "synthesis",
                    SynthesisUpdate {
                        run: run.clone(),
                        path,
                        ok,
                        synthesis,
                        source,
                        error,
                    },
                );
            },
        );
        if let Some(c) = &claude {
            remember_claude_limits(&store, c);
        }
        println!(
            "synthesize ({agent}): {} projects, ran {}, cached {}, next_md {}, failed {}, batches {}",
            jobs.len(),
            summary.ran,
            summary.cached,
            summary.next_md,
            summary.failed,
            summary.batches
        );
        Ok(summary)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Serialize)]
struct PickerInfo {
    /// `agent.default` from the config: `ask` shows the picker, otherwise that agent runs.
    default_agent: config::DefaultAgent,
    meters: logpose_core::usage::UsageMeters,
    /// Which agent CLIs exist on this machine (AGENT-002).
    found: AgentPrograms,
}

/// The configured program per agent (empty = find it) and where each was found (AGENT-001).
#[derive(Debug, Serialize)]
struct AgentPrograms {
    claude_code: Option<String>,
    codex: Option<String>,
    configured_claude: String,
    configured_codex: String,
}

fn agent_programs_for(config: &Config) -> AgentPrograms {
    let found = |a, c: &str| logpose_core::launcher::find_agent(a, c).map(|p| p.display().to_string());
    AgentPrograms {
        claude_code: found(Agent::ClaudeCode, &config.agent.program_claude),
        codex: found(Agent::Codex, &config.agent.program_codex),
        configured_claude: config.agent.program_claude.clone(),
        configured_codex: config.agent.program_codex.clone(),
    }
}

/// The user's extra commit emails for the stats (STATS-005).
#[tauri::command]
fn stats_emails() -> Result<Vec<String>, String> {
    let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
    Ok(config.stats.emails)
}

/// Save the extra commit emails; returns them as stored.
#[tauri::command]
fn set_stats_emails(emails: Vec<String>) -> Result<Vec<String>, String> {
    config::save_stats_emails(&config_path()?, &emails).map_err(|e| e.to_string())?;
    stats_emails()
}

/// Where the agent CLIs are (Settings).
#[tauri::command]
fn agent_programs() -> Result<AgentPrograms, String> {
    let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
    Ok(agent_programs_for(&config))
}

/// Save the agent program overrides (empty = find automatically) and report where they resolve.
#[tauri::command]
fn set_agent_programs(claude: String, codex: String) -> Result<AgentPrograms, String> {
    let path = config_path()?;
    config::save_agent_programs(&path, &claude, &codex).map_err(|e| e.to_string())?;
    agent_programs()
}

/// Usage meters for the startup agent picker (SYN-009); reads the session logs again.
#[tauri::command]
async fn agent_picker(db: State<'_, Db>) -> Result<PickerInfo, String> {
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
        let logs = LogSources::defaults();
        let mut sessions = logs
            .claude
            .as_deref()
            .map(|b| claude::read_sessions(b).0)
            .unwrap_or_default();
        let codex_scan = logs
            .codex
            .as_deref()
            .map(codex::read_sessions)
            .unwrap_or_default();
        sessions.extend(codex_scan.sessions);
        Ok(PickerInfo {
            found: agent_programs_for(&config),
            default_agent: config.agent.default,
            meters: logpose_core::usage::meters(
                &sessions,
                codex_scan.rate_limit.as_ref(),
                db.with(|s| logpose_core::usage::load_claude_limits(s))?,
                Utc::now(),
            ),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Re-apply the suggested placement to every card of the last scan; returns the new board.
#[tauri::command]
fn restore_placement(db: State<'_, Db>, last: State<'_, LastScan>) -> Result<Vec<ProjectRow>, String> {
    let guard = last.0.lock().map_err(|_| "scan lock poisoned".to_string())?;
    let report = guard.as_ref().ok_or("scan first")?;
    db.with(|s| {
        logpose_core::board::apply_suggested(s, &report.projects, Utc::now())?;
        s.projects()
    })
}

/// Set or clear a project's one-line parking note (DETAIL-002); returns the board.
#[tauri::command]
fn set_parking_note(db: State<'_, Db>, id: i64, note: Option<String>) -> Result<Vec<ProjectRow>, String> {
    db.with(|s| {
        s.set_parking_note(id, note.as_deref())?;
        s.projects()
    })
}

/// Turn a project's privacy opt-out on or off (SYN-008): prompts and agent messages stay out of
/// its context pack. Returns the board.
#[tauri::command]
fn set_private(db: State<'_, Db>, id: i64, private: bool) -> Result<Vec<ProjectRow>, String> {
    db.with(|s| {
        s.set_private(id, private)?;
        s.projects()
    })
}

#[derive(Debug, Serialize)]
struct Opened {
    program: String,
    kind: logpose_core::launcher::ProjectKind,
    /// The preferred editor was missing, so the directory opener was used.
    fell_back: bool,
}

/// Open a project with its editor or the file manager per DEC-005 (OPEN-001).
#[tauri::command]
fn open_project(path: String) -> Result<Opened, String> {
    let dir = project_dir(&path)?;
    let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
    let cmd = logpose_core::launcher::open_command(&dir, &config.open)
        .ok_or("no editor or file manager found on PATH")?;
    logpose_core::launcher::spawn_detached(&cmd)
        .map_err(|e| format!("cannot start {}: {e}", cmd.program.display()))?;
    println!("open: {} {}", cmd.program.display(), dir.display());
    Ok(Opened {
        program: cmd.label(),
        kind: cmd.kind,
        fell_back: cmd.fell_back,
    })
}

/// Ask Claude Code for the current plan usage with a tiny call ("Check now" in the picker).
#[tauri::command]
async fn probe_claude_limits(db: State<'_, Db>) -> Result<runner::ClaudeLimits, String> {
    let db = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
        let c = claude_runner(&config)?;
        let limits = c.probe_limits().map_err(|e| e.to_string())?;
        db.with(|s| logpose_core::usage::save_claude_limits(s, &limits))?;
        Ok(limits)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Start Claude Code or Codex interactively in a terminal in the project, with the project's
/// summary (if any) as the first message (LATER-002). Returns the terminal used.
#[tauri::command]
fn launch_agent(
    db: State<'_, Db>,
    last: State<'_, LastScan>,
    path: String,
    agent: String,
) -> Result<String, String> {
    use logpose_core::launcher;
    let dir = project_dir(&path)?;
    let config = Config::load(&config_path()?).map_err(|e| e.to_string())?;
    let find = |name: &str| launcher::find_program(name, Some(launcher::search_path()));
    let (which, configured) = match agent.as_str() {
        "claude-code" => (Agent::ClaudeCode, &config.agent.program_claude),
        "codex" => (Agent::Codex, &config.agent.program_codex),
        other => return Err(format!("unknown agent: {other}")),
    };
    let program = launcher::find_agent(which, configured)
        .ok_or_else(|| launcher::agent_not_found(which, configured))?;
    // The project's summary, from NEXT.md or the cache (no agent call).
    let known = {
        let guard = last.0.lock().map_err(|_| "scan lock poisoned".to_string())?;
        match guard.as_ref() {
            Some(report) => db.with(|s| {
                let jobs = jobs_for(
                    s,
                    report,
                    Some(std::slice::from_ref(&path)),
                    config.agent.context_budget_tokens,
                    false,
                )?;
                Ok(jobs.first().and_then(|j| synth::known(s, j)))
            })?,
            None => None,
        }
    };
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let prompt = match &known {
        Some(k) => synthesis::handoff_prompt(&k.synthesis, &name),
        None => format!(
            "Look at this project ({name}) and tell me where it stands and what the next step should be. \
             Don't change anything yet."
        ),
    };
    let mut cmd = launcher::agent_terminal(
        &logpose_core::platform::Platform::current(),
        config.open.terminal.as_deref(),
        &find,
        &dir,
        &program,
        &[prompt.into()],
    )
    .ok_or("no terminal found; set `terminal` under [open] in config.toml")?;
    cmd.path_env = Some(launcher::child_path(&program));
    launcher::spawn_detached_in(&cmd, &dir)
        .map_err(|e| format!("cannot start {}: {e}", cmd.program.display()))?;
    println!(
        "launch: {agent} in {} via {}",
        dir.display(),
        cmd.program.display()
    );
    Ok(cmd.terminal_label())
}

/// Board rows (column + order) for every known project (BOARD-001).
#[tauri::command]
fn board(db: State<'_, Db>) -> Result<Vec<ProjectRow>, String> {
    db.with(|s| s.projects())
}

/// Move a card into `column` before the card `before` (or to the end), returning the new board.
#[tauri::command]
fn move_card(
    db: State<'_, Db>,
    id: i64,
    column: BoardColumn,
    before: Option<i64>,
) -> Result<Vec<ProjectRow>, String> {
    db.with(|s| {
        s.move_before(id, column, before, Utc::now())?;
        s.projects()
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(Db::default())
        .manage(LastScan::default())
        .manage(LastPlan::default())
        .manage(Tray::default())
        .setup(|app| {
            use tauri::Manager;
            let on = app
                .state::<Db>()
                .with(|s| s.setting(TRAY_KEY))
                .ok()
                .flatten()
                .as_deref()
                == Some("true");
            app.state::<Tray>()
                .on
                .store(on, std::sync::atomic::Ordering::SeqCst);
            if on {
                add_tray(app.handle())?;
            }
            fit_window(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            use tauri::Manager;
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.app_handle().state::<Tray>().is_on()
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            agent_programs,
            set_agent_programs,
            stats_emails,
            set_stats_emails,
            removal_plan,
            check_remote,
            remove_project,
            get_setup,
            save_roots,
            get_settings,
            save_settings,
            scan,
            board,
            move_card,
            recent_commits,
            check_agent,
            agent_rule_status,
            set_agent_rule,
            digest_if_due,
            digest_enabled,
            set_digest_enabled,
            known_syntheses,
            synthesize,
            agent_picker,
            probe_claude_limits,
            restore_placement,
            set_parking_note,
            set_private,
            open_project,
            launch_agent,
            tray_enabled,
            set_tray_enabled,
            stats
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_fits_the_work_area() {
        // Full HD: the ideal width (all five columns + sidebar) with room to spare.
        assert_eq!(fit_window_size(1920.0, 1050.0), (IDEAL_WIDTH + 40.0, 900.0));
        // 1366×768 laptop: limited to 96 % × 92 % of the work area.
        assert_eq!(fit_window_size(1366.0, 740.0), (1311.0, 681.0));
        // Tiny screen: never larger than the screen.
        assert_eq!(fit_window_size(800.0, 500.0), (800.0, 500.0));
    }
}
