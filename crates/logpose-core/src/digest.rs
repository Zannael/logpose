//! Weekly digest (LATER-004, DEC-006): the 3 most neglected projects, shown when the app is opened
//! at most once a week (there is no always-on mode), and switchable off in settings.
//!
//! "Neglected" = the stalest projects still in play: Archived and Maintenance cards are parked on
//! purpose and never listed. Ties go to the messier project.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::store::{BoardColumn, ProjectRow, Snapshot, Store};

/// Settings keys in the store.
pub const ENABLED_KEY: &str = "digest.enabled";
pub const LAST_SHOWN_KEY: &str = "digest.last_shown";
pub const INTERVAL: Duration = Duration::days(7);
pub const COUNT: usize = 3;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Neglected {
    pub project_id: i64,
    pub name: String,
    pub column: BoardColumn,
    pub staleness_days: f64,
    pub mess_score: Option<f64>,
}

/// Pick up to `n` projects by staleness (then mess), skipping Archived/Maintenance and projects
/// without a known staleness. `latest` holds each project's most recent snapshot.
pub fn pick(rows: &[ProjectRow], latest: &[Snapshot], n: usize) -> Vec<Neglected> {
    let mut out: Vec<Neglected> = rows
        .iter()
        .filter(|r| !matches!(r.column, BoardColumn::Archived | BoardColumn::Maintenance))
        .filter_map(|r| {
            let s = latest
                .iter()
                .filter(|s| s.project_id == r.id)
                .max_by_key(|s| s.taken_at)?;
            Some(Neglected {
                project_id: r.id,
                name: r.alias.clone().unwrap_or_else(|| r.title.clone()),
                column: r.column,
                staleness_days: s.staleness_days?,
                mess_score: s.mess_score,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.staleness_days
            .total_cmp(&a.staleness_days)
            .then(
                b.mess_score
                    .unwrap_or(0.0)
                    .total_cmp(&a.mess_score.unwrap_or(0.0)),
            )
            .then(a.name.cmp(&b.name))
    });
    out.truncate(n);
    out
}

/// Due when never shown or shown at least [`INTERVAL`] ago.
pub fn due(last_shown: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_shown.is_none_or(|t| now - t >= INTERVAL)
}

/// Notification text: one line per project.
pub fn body(items: &[Neglected]) -> String {
    items
        .iter()
        .map(|i| {
            format!(
                "{}: untouched for {} days",
                i.name,
                i.staleness_days.round() as i64
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn enabled(store: &Store) -> rusqlite::Result<bool> {
    Ok(store.setting(ENABLED_KEY)?.as_deref() != Some("false"))
}

pub fn set_enabled(store: &Store, on: bool) -> rusqlite::Result<()> {
    store.set_setting(ENABLED_KEY, if on { "true" } else { "false" })
}

/// When enabled and due, pick the projects from the latest snapshots, record `now` as shown and
/// return them (empty when nothing qualifies, in which case nothing is recorded).
pub fn take_if_due(store: &Store, now: DateTime<Utc>) -> rusqlite::Result<Option<Vec<Neglected>>> {
    if !enabled(store)? {
        return Ok(None);
    }
    let last = store
        .setting(LAST_SHOWN_KEY)?
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc));
    if !due(last, now) {
        return Ok(None);
    }
    let items = pick(
        &store.projects()?,
        &store.snapshots_since(now - Duration::days(2))?,
        COUNT,
    );
    if items.is_empty() {
        return Ok(None);
    }
    store.set_setting(LAST_SHOWN_KEY, &now.to_rfc3339())?;
    Ok(Some(items))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn seed(store: &mut Store, now: DateTime<Utc>) -> Vec<i64> {
        // name, staleness, mess, column
        let projects = [
            ("fresh", 1.0, 10.0, BoardColumn::Now),
            ("old-a", 90.0, 20.0, BoardColumn::Next),
            ("old-b", 90.0, 60.0, BoardColumn::Paused),
            ("parked", 400.0, 0.0, BoardColumn::Archived),
            ("finished", 300.0, 0.0, BoardColumn::Maintenance),
            ("mid", 40.0, 5.0, BoardColumn::Next),
        ];
        let mut ids = Vec::new();
        for (name, days, mess, col) in projects {
            let id = store
                .upsert_project(Path::new(&format!("/p/{name}")), name, now)
                .unwrap();
            store.move_before(id, col, None, now).unwrap();
            store
                .insert_snapshot(&Snapshot {
                    project_id: id,
                    taken_at: now,
                    staleness_days: Some(days),
                    mess_score: Some(mess),
                    signals_json: "{}".into(),
                })
                .unwrap();
            ids.push(id);
        }
        ids
    }

    #[test]
    fn picks_stalest_active_projects_and_breaks_ties_by_mess() {
        let now = t("2026-11-02T10:00:00Z");
        let mut store = Store::open_in_memory().unwrap();
        seed(&mut store, now);
        let items = pick(
            &store.projects().unwrap(),
            &store.snapshots_since(now).unwrap(),
            3,
        );
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["old-b", "old-a", "mid"]);
        assert_eq!(body(&items[..1]), "old-b: untouched for 90 days");
    }

    #[test]
    fn weekly_timing_and_off_switch() {
        let now = t("2026-11-02T10:00:00Z");
        let mut store = Store::open_in_memory().unwrap();
        seed(&mut store, now);
        assert!(enabled(&store).unwrap(), "on by default (DEC-006)");
        assert_eq!(take_if_due(&store, now).unwrap().unwrap().len(), 3);
        assert!(
            take_if_due(&store, now + Duration::days(6)).unwrap().is_none(),
            "shown 6 days ago"
        );
        assert!(due(Some(now), now + Duration::days(7)));
        set_enabled(&store, false).unwrap();
        assert!(take_if_due(&store, now + Duration::days(30)).unwrap().is_none());
        set_enabled(&store, true).unwrap();
        // Snapshots older than 2 days are not used: nothing qualifies, nothing is recorded.
        assert!(take_if_due(&store, now + Duration::days(30)).unwrap().is_none());
        assert_eq!(store.setting(LAST_SHOWN_KEY).unwrap(), Some(now.to_rfc3339()));
    }
}
