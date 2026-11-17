//! Local SQLite store (BLUEPRINT §10, CORE-003).
//!
//! Lives in the app data dir (`<Platform::data_dir()>/logpose/logpose.sqlite`). Schema changes are
//! numbered migrations tracked with `PRAGMA user_version`; never edit a shipped migration, append one.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub type Result<T> = rusqlite::Result<T>;

/// Longest parking note kept (DETAIL-002).
pub const PARKING_NOTE_MAX_CHARS: usize = 200;

const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    r#"
    CREATE TABLE projects (
        id           INTEGER PRIMARY KEY,
        path         TEXT NOT NULL UNIQUE,
        title        TEXT NOT NULL,
        alias        TEXT,
        board_column TEXT NOT NULL DEFAULT 'next'
                     CHECK (board_column IN ('now', 'next', 'paused', 'maintenance', 'archived')),
        position     INTEGER NOT NULL DEFAULT 0,
        archived_at  TEXT,
        parking_note TEXT,
        created_at   TEXT NOT NULL
    );
    CREATE TABLE snapshots (
        id             INTEGER PRIMARY KEY,
        project_id     INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        taken_at       TEXT NOT NULL,
        staleness_days REAL,
        mess_score     REAL,
        signals_json   TEXT NOT NULL DEFAULT '{}'
    );
    CREATE INDEX snapshots_project_time ON snapshots(project_id, taken_at);
    CREATE TABLE synthesis_cache (
        id          INTEGER PRIMARY KEY,
        project_id  INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        cache_key   TEXT NOT NULL,
        agent       TEXT NOT NULL,
        model       TEXT,
        result_json TEXT NOT NULL,
        tokens_in   INTEGER NOT NULL DEFAULT 0,
        tokens_out  INTEGER NOT NULL DEFAULT 0,
        created_at  TEXT NOT NULL,
        UNIQUE (project_id, cache_key, agent)
    );
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    // 2: per-project privacy opt-out (SYN-008): keep prompts and agent messages out of context packs.
    "ALTER TABLE projects ADD COLUMN private INTEGER NOT NULL DEFAULT 0;",
    // 3: when a project entered Maintenance ("finished", DEC-004), for the per-month stats (STATS-003).
    "ALTER TABLE projects ADD COLUMN maintenance_at TEXT;",
];

/// Rows removed by [`Store::prune`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    pub projects: usize,
    pub summaries: usize,
    pub snapshots: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoardColumn {
    Now,
    Next,
    Paused,
    Maintenance,
    Archived,
}

impl BoardColumn {
    pub const ALL: [BoardColumn; 5] = [
        Self::Now,
        Self::Next,
        Self::Paused,
        Self::Maintenance,
        Self::Archived,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::Next => "next",
            Self::Paused => "paused",
            Self::Maintenance => "maintenance",
            Self::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectRow {
    pub id: i64,
    pub path: PathBuf,
    pub title: String,
    pub alias: Option<String>,
    pub column: BoardColumn,
    pub position: i64,
    pub archived_at: Option<DateTime<Utc>>,
    /// When the project entered Maintenance; cleared on leaving it (like `archived_at`).
    pub maintenance_at: Option<DateTime<Utc>>,
    pub parking_note: Option<String>,
    /// Privacy opt-out (SYN-008): no prompts or agent messages in this project's context pack.
    pub private: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub project_id: i64,
    pub taken_at: DateTime<Utc>,
    pub staleness_days: Option<f64>,
    pub mess_score: Option<f64>,
    pub signals_json: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CachedSynthesis {
    pub project_id: i64,
    pub cache_key: String,
    pub agent: String,
    pub model: Option<String>,
    pub result_json: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub created_at: DateTime<Utc>,
}

pub struct Store {
    conn: Connection,
}

/// `<platform data dir>/logpose/logpose.sqlite`.
pub fn default_path() -> Option<PathBuf> {
    crate::platform::Platform::current()
        .data_dir()
        .map(|d| d.join("logpose").join("logpose.sqlite"))
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

fn parse_ts(s: Option<String>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&s?)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn schema_version(&self) -> Result<usize> {
        self.conn
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .map(|v| v as usize)
    }

    fn migrate(&mut self) -> Result<()> {
        let current = self.schema_version()?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Insert a project if its path is new (landing at the end of `Next`), refresh its title, return its id.
    pub fn upsert_project(&self, path: &Path, title: &str, now: DateTime<Utc>) -> Result<i64> {
        let path_s = path.to_string_lossy();
        if let Some(id) = self
            .conn
            .query_row("SELECT id FROM projects WHERE path = ?1", [&path_s], |r| r.get(0))
            .optional()?
        {
            self.conn
                .execute("UPDATE projects SET title = ?2 WHERE id = ?1", params![id, title])?;
            return Ok(id);
        }
        let position = self.next_position(BoardColumn::Next)?;
        self.conn.execute(
            "INSERT INTO projects (path, title, board_column, position, created_at) VALUES (?1, ?2, 'next', ?3, ?4)",
            params![path_s, title, position, ts(now)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn next_position(&self, column: BoardColumn) -> Result<i64> {
        self.conn.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM projects WHERE board_column = ?1",
            [column.as_str()],
            |r| r.get(0),
        )
    }

    fn row(r: &rusqlite::Row) -> Result<ProjectRow> {
        let column: String = r.get(4)?;
        Ok(ProjectRow {
            id: r.get(0)?,
            path: PathBuf::from(r.get::<_, String>(1)?),
            title: r.get(2)?,
            alias: r.get(3)?,
            column: BoardColumn::parse(&column).unwrap_or(BoardColumn::Next),
            position: r.get(5)?,
            archived_at: parse_ts(r.get(6)?),
            parking_note: r.get(7)?,
            private: r.get(8)?,
            maintenance_at: parse_ts(r.get(9)?),
        })
    }

    const SELECT: &'static str = "SELECT id, path, title, alias, board_column, position, archived_at, parking_note, private, maintenance_at FROM projects";

    pub fn project(&self, id: i64) -> Result<Option<ProjectRow>> {
        self.conn
            .query_row(&format!("{} WHERE id = ?1", Self::SELECT), [id], Self::row)
            .optional()
    }

    /// All projects ordered by board column, then position.
    pub fn projects(&self) -> Result<Vec<ProjectRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "{} ORDER BY CASE board_column WHEN 'now' THEN 0 WHEN 'next' THEN 1 WHEN 'paused' THEN 2 \
             WHEN 'maintenance' THEN 3 ELSE 4 END, position, id",
            Self::SELECT
        ))?;
        stmt.query_map([], Self::row)?.collect()
    }

    /// Move a project to `column` at `position`, shifting others; sets/clears `archived_at` and
    /// `maintenance_at`.
    pub fn move_project(
        &mut self,
        id: i64,
        column: BoardColumn,
        position: i64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let (old_col, old_pos): (String, i64) = tx.query_row(
            "SELECT board_column, position FROM projects WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "UPDATE projects SET position = position - 1 WHERE board_column = ?1 AND position > ?2",
            params![old_col, old_pos],
        )?;
        tx.execute(
            "UPDATE projects SET position = position + 1 WHERE board_column = ?1 AND position >= ?2 AND id != ?3",
            params![column.as_str(), position, id],
        )?;
        tx.execute(
            "UPDATE projects SET board_column = ?2, position = ?3, \
             archived_at = CASE WHEN ?2 = 'archived' THEN COALESCE(archived_at, ?4) ELSE NULL END, \
             maintenance_at = CASE WHEN ?2 = 'maintenance' THEN COALESCE(maintenance_at, ?4) ELSE NULL END \
             WHERE id = ?1",
            params![id, column.as_str(), position, ts(now)],
        )?;
        tx.commit()
    }

    /// Move a project into `column` just before the project `before` (or to the end of the column when
    /// `None` or when `before` is not in `column`). Positions come from the rows themselves, so rows the UI
    /// doesn't show (projects missing from the current scan) don't skew the index.
    pub fn move_before(
        &mut self,
        id: i64,
        column: BoardColumn,
        before: Option<i64>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let moving = self.project(id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let target = match before.filter(|b| *b != id) {
            Some(b) => self.project(b)?.filter(|r| r.column == column),
            None => None,
        };
        let same_column = moving.column == column;
        let position = match target {
            // Removing the moving row first shifts later rows of the same column up by one.
            Some(t) if same_column && moving.position < t.position => t.position - 1,
            Some(t) => t.position,
            None if same_column => self.next_position(column)? - 1,
            None => self.next_position(column)?,
        };
        self.move_project(id, column, position, now)
    }

    /// Put each `(id, column)` in that column, in the given order (positions 0, 1, …); rows not listed
    /// keep their column and follow after (BOARD-003). Archive/maintenance timestamps follow the column.
    pub fn place_all(&mut self, placement: &[(i64, BoardColumn)], now: DateTime<Utc>) -> Result<()> {
        let tx = self.conn.transaction()?;
        let listed: std::collections::HashSet<i64> = placement.iter().map(|(id, _)| *id).collect();
        let mut next = std::collections::HashMap::<&str, i64>::new();
        for (id, column) in placement {
            let pos = next.entry(column.as_str()).or_insert(0);
            tx.execute(
                "UPDATE projects SET board_column = ?2, position = ?3, \
                 archived_at = CASE WHEN ?2 = 'archived' THEN COALESCE(archived_at, ?4) ELSE NULL END, \
                 maintenance_at = CASE WHEN ?2 = 'maintenance' THEN COALESCE(maintenance_at, ?4) ELSE NULL END \
                 WHERE id = ?1",
                params![id, column.as_str(), *pos, ts(now)],
            )?;
            *pos += 1;
        }
        // Unlisted rows (not in this scan) go after the listed ones, keeping their order.
        let mut rest: Vec<(i64, String, i64)> = {
            let mut stmt =
                tx.prepare("SELECT id, board_column, position FROM projects ORDER BY position, id")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_>>()?
        };
        rest.retain(|(id, _, _)| !listed.contains(id));
        for (id, column, _) in rest {
            let pos = next
                .entry(BoardColumn::parse(&column).unwrap_or(BoardColumn::Next).as_str())
                .or_insert(0);
            tx.execute(
                "UPDATE projects SET position = ?2 WHERE id = ?1",
                params![id, *pos],
            )?;
            *pos += 1;
        }
        tx.commit()
    }

    /// One line (whitespace and line breaks collapse to single spaces), at most
    /// [`PARKING_NOTE_MAX_CHARS`]; empty clears it.
    pub fn set_parking_note(&self, id: i64, note: Option<&str>) -> Result<()> {
        let note = note
            .map(|n| {
                let line = n.split_whitespace().collect::<Vec<_>>().join(" ");
                line.chars().take(PARKING_NOTE_MAX_CHARS).collect::<String>()
            })
            .filter(|n| !n.is_empty());
        self.conn.execute(
            "UPDATE projects SET parking_note = ?2 WHERE id = ?1",
            params![id, note],
        )?;
        Ok(())
    }

    pub fn set_private(&self, id: i64, private: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET private = ?2 WHERE id = ?1",
            params![id, private],
        )?;
        Ok(())
    }

    pub fn set_alias(&self, id: i64, alias: Option<&str>) -> Result<()> {
        self.conn
            .execute("UPDATE projects SET alias = ?2 WHERE id = ?1", params![id, alias])?;
        Ok(())
    }

    pub fn insert_snapshot(&self, s: &Snapshot) -> Result<()> {
        self.conn.execute(
            "INSERT INTO snapshots (project_id, taken_at, staleness_days, mess_score, signals_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![s.project_id, ts(s.taken_at), s.staleness_days, s.mess_score, s.signals_json],
        )?;
        Ok(())
    }

    pub fn snapshots(&self, project_id: i64) -> Result<Vec<Snapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT project_id, taken_at, staleness_days, mess_score, signals_json FROM snapshots \
             WHERE project_id = ?1 ORDER BY taken_at",
        )?;
        stmt.query_map([project_id], |r| {
            Ok(Snapshot {
                project_id: r.get(0)?,
                taken_at: parse_ts(r.get(1)?).unwrap_or_default(),
                staleness_days: r.get(2)?,
                mess_score: r.get(3)?,
                signals_json: r.get(4)?,
            })
        })?
        .collect()
    }

    /// Every snapshot taken at or after `since`, ordered by time (STATS-003).
    pub fn snapshots_since(&self, since: DateTime<Utc>) -> Result<Vec<Snapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT project_id, taken_at, staleness_days, mess_score, signals_json FROM snapshots \
             WHERE taken_at >= ?1 ORDER BY taken_at, project_id",
        )?;
        stmt.query_map([ts(since)], |r| {
            Ok(Snapshot {
                project_id: r.get(0)?,
                taken_at: parse_ts(r.get(1)?).unwrap_or_default(),
                staleness_days: r.get(2)?,
                mess_score: r.get(3)?,
                signals_json: r.get(4)?,
            })
        })?
        .collect()
    }

    pub fn put_synthesis(&self, c: &CachedSynthesis) -> Result<()> {
        self.conn.execute(
            "INSERT INTO synthesis_cache (project_id, cache_key, agent, model, result_json, tokens_in, tokens_out, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT (project_id, cache_key, agent) DO UPDATE SET model = excluded.model, \
             result_json = excluded.result_json, tokens_in = excluded.tokens_in, tokens_out = excluded.tokens_out, \
             created_at = excluded.created_at",
            params![
                c.project_id,
                c.cache_key,
                c.agent,
                c.model,
                c.result_json,
                c.tokens_in as i64,
                c.tokens_out as i64,
                ts(c.created_at)
            ],
        )?;
        Ok(())
    }

    /// Cached synthesis for this exact key (any agent, most recent first).
    pub fn cached_synthesis(&self, project_id: i64, cache_key: &str) -> Result<Option<CachedSynthesis>> {
        self.conn
            .query_row(
                "SELECT project_id, cache_key, agent, model, result_json, tokens_in, tokens_out, created_at \
                 FROM synthesis_cache WHERE project_id = ?1 AND cache_key = ?2 ORDER BY created_at DESC LIMIT 1",
                params![project_id, cache_key],
                |r| {
                    Ok(CachedSynthesis {
                        project_id: r.get(0)?,
                        cache_key: r.get(1)?,
                        agent: r.get(2)?,
                        model: r.get(3)?,
                        result_json: r.get(4)?,
                        tokens_in: r.get::<_, i64>(5)? as u64,
                        tokens_out: r.get::<_, i64>(6)? as u64,
                        created_at: parse_ts(r.get(7)?).unwrap_or_default(),
                    })
                },
            )
            .optional()
    }

    /// The most recent cached synthesis of a project, whatever its key or agent (SYN-010).
    pub fn latest_synthesis(&self, project_id: i64) -> Result<Option<CachedSynthesis>> {
        self.conn
            .query_row(
                "SELECT project_id, cache_key, agent, model, result_json, tokens_in, tokens_out, created_at \
                 FROM synthesis_cache WHERE project_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1",
                params![project_id],
                |r| {
                    Ok(CachedSynthesis {
                        project_id: r.get(0)?,
                        cache_key: r.get(1)?,
                        agent: r.get(2)?,
                        model: r.get(3)?,
                        result_json: r.get(4)?,
                        tokens_in: r.get::<_, i64>(5)? as u64,
                        tokens_out: r.get::<_, i64>(6)? as u64,
                        created_at: parse_ts(r.get(7)?).unwrap_or_default(),
                    })
                },
            )
            .optional()
    }

    /// Drop a project and, by cascade, its snapshots and cached summaries (DEL-002). Returns
    /// whether it was known.
    pub fn forget_project(&self, path: &Path) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM projects WHERE path = ?1", [path.to_string_lossy()])?
            > 0)
    }

    /// Clean up data that no longer serves anything (DEL-004):
    /// - projects for which `gone(path)` holds (the app passes: the folder is missing but the folder
    ///   containing it exists, so an unmounted disk never wipes the board);
    /// - cached summaries other than the latest per project and agent (older cache keys are never
    ///   looked up again);
    /// - snapshots older than two days except the last one per project and local day in `tz`
    ///   (all that the Stats staleness series uses).
    pub fn prune<Tz: chrono::TimeZone>(
        &mut self,
        now: DateTime<Utc>,
        tz: &Tz,
        gone: &dyn Fn(&Path) -> bool,
    ) -> Result<PruneReport> {
        let mut report = PruneReport::default();
        let rows = self.projects()?;
        let tx = self.conn.transaction()?;
        for row in rows.iter().filter(|r| gone(&r.path)) {
            report.projects += tx.execute("DELETE FROM projects WHERE id = ?1", [row.id])?;
        }
        report.summaries = tx.execute(
            "DELETE FROM synthesis_cache WHERE id NOT IN ( \
               SELECT id FROM ( \
                 SELECT id, ROW_NUMBER() OVER (PARTITION BY project_id, agent ORDER BY created_at DESC, id DESC) AS n \
                 FROM synthesis_cache) WHERE n = 1)",
            [],
        )?;
        let cutoff = now - chrono::Duration::days(2);
        let old: Vec<(i64, i64, Option<DateTime<Utc>>)> = {
            let mut stmt =
                tx.prepare("SELECT id, project_id, taken_at FROM snapshots WHERE taken_at < ?1")?;
            stmt.query_map([ts(cutoff)], |r| Ok((r.get(0)?, r.get(1)?, parse_ts(r.get(2)?))))?
                .collect::<Result<_>>()?
        };
        let mut last: std::collections::HashMap<(i64, chrono::NaiveDate), (DateTime<Utc>, i64)> =
            std::collections::HashMap::new();
        for (id, project, at) in &old {
            let Some(at) = at else { continue };
            let key = (*project, at.with_timezone(tz).date_naive());
            let e = last.entry(key).or_insert((*at, *id));
            if (*at, *id) > *e {
                *e = (*at, *id);
            }
        }
        let keep: std::collections::HashSet<i64> = last.values().map(|(_, id)| *id).collect();
        for (id, _, _) in old
            .iter()
            .filter(|(id, _, at)| at.is_some() && !keep.contains(id))
        {
            report.snapshots += tx.execute("DELETE FROM snapshots WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        if report.snapshots + report.summaries > 500 {
            self.conn.execute_batch("VACUUM")?;
        }
        Ok(report)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn now() -> DateTime<Utc> {
        t("2026-11-01T10:00:00Z")
    }

    #[test]
    fn migrations_are_idempotent_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/logpose.sqlite");
        let id = {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len());
            s.upsert_project(Path::new("/p/a"), "a", now()).unwrap()
        };
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len());
        assert_eq!(s.project(id).unwrap().unwrap().title, "a");
    }

    #[test]
    fn v1_database_upgrades_with_privacy_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logpose.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO projects (path, title, created_at) VALUES ('/p/a', 'a', '2026-11-01T10:00:00+00:00')",
                [],
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), MIGRATIONS.len());
        let a = &s.projects().unwrap()[0];
        assert!(!a.private);
        s.set_private(a.id, true).unwrap();
        assert!(s.project(a.id).unwrap().unwrap().private);
    }

    #[test]
    fn new_projects_land_in_next_and_upsert_is_stable() {
        let s = Store::open_in_memory().unwrap();
        let a = s.upsert_project(Path::new("/p/a"), "a", now()).unwrap();
        let b = s.upsert_project(Path::new("/p/b"), "b", now()).unwrap();
        assert_eq!(s.upsert_project(Path::new("/p/a"), "a2", now()).unwrap(), a);
        let rows = s.projects().unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (r.id, r.column, r.position))
                .collect::<Vec<_>>(),
            [(a, BoardColumn::Next, 0), (b, BoardColumn::Next, 1)]
        );
        assert_eq!(rows[0].title, "a2");
    }

    #[test]
    fn moving_reorders_and_tracks_archive_time() {
        let mut s = Store::open_in_memory().unwrap();
        let ids: Vec<i64> = ["a", "b", "c"]
            .iter()
            .map(|n| s.upsert_project(Path::new(&format!("/p/{n}")), n, now()).unwrap())
            .collect();
        s.move_project(ids[2], BoardColumn::Next, 0, now()).unwrap();
        let order: Vec<i64> = s.projects().unwrap().iter().map(|r| r.id).collect();
        assert_eq!(order, [ids[2], ids[0], ids[1]]);
        let positions: Vec<i64> = s.projects().unwrap().iter().map(|r| r.position).collect();
        assert_eq!(positions, [0, 1, 2]);

        s.move_project(ids[0], BoardColumn::Archived, 0, now()).unwrap();
        let a = s.project(ids[0]).unwrap().unwrap();
        assert_eq!((a.column, a.archived_at), (BoardColumn::Archived, Some(now())));
        let next: Vec<i64> = s
            .projects()
            .unwrap()
            .iter()
            .filter(|r| r.column == BoardColumn::Next)
            .map(|r| r.position)
            .collect();
        assert_eq!(next, [0, 1]);
        s.move_project(ids[0], BoardColumn::Maintenance, 0, now())
            .unwrap();
        let m = s.project(ids[0]).unwrap().unwrap();
        assert_eq!((m.archived_at, m.maintenance_at), (None, Some(now())));
        s.move_project(ids[0], BoardColumn::Maintenance, 1, t("2026-11-05T10:00:00Z"))
            .unwrap();
        assert_eq!(
            s.project(ids[0]).unwrap().unwrap().maintenance_at,
            Some(now()),
            "kept while staying"
        );
        s.move_project(ids[0], BoardColumn::Now, 0, now()).unwrap();
        assert_eq!(s.project(ids[0]).unwrap().unwrap().maintenance_at, None);
    }

    fn column(s: &Store, c: BoardColumn) -> Vec<i64> {
        s.projects()
            .unwrap()
            .iter()
            .filter(|r| r.column == c)
            .map(|r| r.id)
            .collect()
    }

    #[test]
    fn move_before_places_relative_to_neighbours() {
        let mut s = Store::open_in_memory().unwrap();
        let [a, b, c, d] =
            ["a", "b", "c", "d"].map(|n| s.upsert_project(Path::new(&format!("/p/{n}")), n, now()).unwrap());
        // Down within the column: a before d → b c a d.
        s.move_before(a, BoardColumn::Next, Some(d), now()).unwrap();
        assert_eq!(column(&s, BoardColumn::Next), [b, c, a, d]);
        // Up within the column: d before b → d b c a.
        s.move_before(d, BoardColumn::Next, Some(b), now()).unwrap();
        assert_eq!(column(&s, BoardColumn::Next), [d, b, c, a]);
        // To the end of the same column.
        s.move_before(d, BoardColumn::Next, None, now()).unwrap();
        assert_eq!(column(&s, BoardColumn::Next), [b, c, a, d]);
        // Across columns: end of Now, then before an existing Now card.
        s.move_before(c, BoardColumn::Now, None, now()).unwrap();
        s.move_before(b, BoardColumn::Now, Some(c), now()).unwrap();
        assert_eq!(column(&s, BoardColumn::Now), [b, c]);
        assert_eq!(column(&s, BoardColumn::Next), [a, d]);
        // `before` in another column falls back to the end.
        s.move_before(a, BoardColumn::Now, Some(d), now()).unwrap();
        assert_eq!(column(&s, BoardColumn::Now), [b, c, a]);
        // Moving before itself is a no-op apart from going to the end.
        s.move_before(b, BoardColumn::Now, Some(b), now()).unwrap();
        assert_eq!(column(&s, BoardColumn::Now), [c, a, b]);
        let positions: Vec<i64> = s.projects().unwrap().iter().map(|r| r.position).collect();
        assert_eq!(positions, [0, 1, 2, 0]);
    }

    #[test]
    fn board_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logpose.sqlite");
        let (a, b) = {
            let mut s = Store::open(&path).unwrap();
            let a = s.upsert_project(Path::new("/p/a"), "a", now()).unwrap();
            let b = s.upsert_project(Path::new("/p/b"), "b", now()).unwrap();
            s.move_before(b, BoardColumn::Now, None, now()).unwrap();
            s.move_before(a, BoardColumn::Now, Some(b), now()).unwrap();
            (a, b)
        };
        let s = Store::open(&path).unwrap();
        // A rescan upserts the same paths without moving them.
        s.upsert_project(Path::new("/p/a"), "a", now()).unwrap();
        s.upsert_project(Path::new("/p/c"), "c", now()).unwrap();
        assert_eq!(column(&s, BoardColumn::Now), [a, b]);
        assert_eq!(s.projects().unwrap()[2].column, BoardColumn::Next);
    }

    #[test]
    fn notes_snapshots_cache_settings() {
        let s = Store::open_in_memory().unwrap();
        let id = s.upsert_project(Path::new("/p/a"), "a", now()).unwrap();
        s.set_parking_note(id, Some("  waiting on API key ")).unwrap();
        assert_eq!(
            s.project(id).unwrap().unwrap().parking_note.as_deref(),
            Some("waiting on API key")
        );
        s.set_parking_note(id, Some("line one\n  line two\t end"))
            .unwrap();
        assert_eq!(
            s.project(id).unwrap().unwrap().parking_note.as_deref(),
            Some("line one line two end")
        );
        s.set_parking_note(id, Some(&"é".repeat(300))).unwrap();
        assert_eq!(
            s.project(id)
                .unwrap()
                .unwrap()
                .parking_note
                .unwrap()
                .chars()
                .count(),
            PARKING_NOTE_MAX_CHARS
        );
        s.set_parking_note(id, Some("  ")).unwrap();
        assert_eq!(s.project(id).unwrap().unwrap().parking_note, None);

        for (i, when) in ["2026-10-30T00:00:00Z", "2026-10-31T00:00:00Z"]
            .iter()
            .enumerate()
        {
            s.insert_snapshot(&Snapshot {
                project_id: id,
                taken_at: t(when),
                staleness_days: Some(i as f64),
                mess_score: Some(10.0),
                signals_json: "{}".into(),
            })
            .unwrap();
        }
        assert_eq!(s.snapshots(id).unwrap().len(), 2);

        let mut c = CachedSynthesis {
            project_id: id,
            cache_key: "k1".into(),
            agent: "claude-code".into(),
            model: Some("claude-haiku-5-5".into()),
            result_json: "{}".into(),
            tokens_in: 1600,
            tokens_out: 700,
            created_at: now(),
        };
        s.put_synthesis(&c).unwrap();
        c.tokens_out = 800;
        s.put_synthesis(&c).unwrap();
        assert_eq!(s.cached_synthesis(id, "k1").unwrap().unwrap().tokens_out, 800);
        assert!(s.cached_synthesis(id, "k2").unwrap().is_none());

        assert_eq!(s.setting("agent").unwrap(), None);
        s.set_setting("agent", "codex").unwrap();
        s.set_setting("agent", "claude").unwrap();
        assert_eq!(s.setting("agent").unwrap().as_deref(), Some("claude"));
    }

    #[test]
    fn column_constraint_rejects_unknown_values() {
        let s = Store::open_in_memory().unwrap();
        let id = s.upsert_project(Path::new("/p/a"), "a", now()).unwrap();
        assert!(
            s.conn
                .execute("UPDATE projects SET board_column = 'done' WHERE id = ?1", [id])
                .is_err()
        );
        assert!(
            s.conn
                .execute(
                    "INSERT INTO snapshots (project_id, taken_at) VALUES (999, 'x')",
                    []
                )
                .is_err()
        );
    }

    #[test]
    fn forget_and_prune() {
        let mut store = Store::open_in_memory().unwrap();
        let a = store.upsert_project(Path::new("/r/a"), "a", now()).unwrap();
        let gone = store.upsert_project(Path::new("/r/gone"), "gone", now()).unwrap();
        let unmounted = store.upsert_project(Path::new("/mnt/usb/p"), "p", now()).unwrap();
        let snap = |id: i64, at: &str| Snapshot {
            project_id: id,
            taken_at: t(at),
            staleness_days: Some(1.0),
            mess_score: None,
            signals_json: "{}".into(),
        };
        for at in [
            "2026-10-20T08:00:00Z",
            "2026-10-20T09:00:00Z",
            "2026-10-20T23:30:00Z",
            "2026-10-21T01:00:00Z",
            "2026-10-31T09:00:00Z",
            "2026-10-31T10:00:00Z",
        ] {
            store.insert_snapshot(&snap(a, at)).unwrap();
        }
        store
            .insert_snapshot(&snap(gone, "2026-10-20T08:00:00Z"))
            .unwrap();
        let cache = |id: i64, key: &str, agent: &str, at: &str| CachedSynthesis {
            project_id: id,
            cache_key: key.into(),
            agent: agent.into(),
            model: None,
            result_json: "{}".into(),
            tokens_in: 0,
            tokens_out: 0,
            created_at: t(at),
        };
        store
            .put_synthesis(&cache(a, "k1", "claude-code", "2026-10-20T08:00:00Z"))
            .unwrap();
        store
            .put_synthesis(&cache(a, "k2", "claude-code", "2026-10-25T08:00:00Z"))
            .unwrap();
        store
            .put_synthesis(&cache(a, "k1", "codex", "2026-10-21T08:00:00Z"))
            .unwrap();

        // `gone`: missing folder under a present root; the unmounted one is kept by the caller's rule.
        let r = store
            .prune(now(), &Utc, &|p: &Path| p == Path::new("/r/gone"))
            .unwrap();
        assert_eq!(
            r,
            PruneReport {
                projects: 1,
                summaries: 1,
                snapshots: 2
            }
        );
        let ids: Vec<i64> = store.projects().unwrap().iter().map(|p| p.id).collect();
        assert_eq!(ids, [a, unmounted]);
        // Last snapshot per UTC day before the two-day cutoff; recent ones untouched.
        let kept: Vec<String> = store
            .snapshots(a)
            .unwrap()
            .iter()
            .map(|s| s.taken_at.to_rfc3339())
            .collect();
        assert_eq!(
            kept,
            [
                "2026-10-20T23:30:00+00:00",
                "2026-10-21T01:00:00+00:00",
                "2026-10-31T09:00:00+00:00",
                "2026-10-31T10:00:00+00:00"
            ]
        );
        assert_eq!(store.latest_synthesis(a).unwrap().unwrap().cache_key, "k2");
        assert!(
            store.cached_synthesis(a, "k1").unwrap().is_some(),
            "Codex's latest stays"
        );
        // Idempotent.
        assert_eq!(
            store.prune(now(), &Utc, &|_| false).unwrap(),
            PruneReport::default()
        );

        assert!(store.forget_project(Path::new("/r/a")).unwrap());
        assert!(!store.forget_project(Path::new("/r/a")).unwrap());
        assert!(store.snapshots(a).unwrap().is_empty() && store.latest_synthesis(a).unwrap().is_none());
    }
}
