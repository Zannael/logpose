//! Suggested board placement (BOARD-003, rules approved in DEC-008):
//!
//! - **Now**: the 3 most recently active Fresh projects (< 3 days), ties broken by sessions in the last 7 days.
//! - **Next**: other Fresh and Cooling (3–14 days) projects, and projects without any known activity.
//! - **Paused**: Stale (14–60 days).
//! - **Maintenance**: Dormant (> 60 days) and Clean.
//! - **Archived**: other Dormant projects, including Dormant projects without VCS (no mess label).
//!
//! Within a column, the most recently active project comes first.

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::scan::ProjectReport;
use crate::score::{MessLabel, StalenessBucket};
use crate::store::{BoardColumn, Store};

pub const NOW_SLOTS: usize = 3;

/// `(project path, column)` in board order.
pub fn suggest(projects: &[ProjectReport]) -> Vec<(PathBuf, BoardColumn)> {
    let mut ordered: Vec<&ProjectReport> = projects.iter().collect();
    // Most recent first; unknown staleness last; more recent sessions first on ties.
    ordered.sort_by(|a, b| {
        let days = |p: &ProjectReport| p.staleness.map_or(f64::INFINITY, |s| s.days);
        days(a)
            .total_cmp(&days(b))
            .then_with(|| sessions_7d(b).cmp(&sessions_7d(a)))
            .then_with(|| a.project.path.cmp(&b.project.path))
    });
    let now_paths: Vec<&PathBuf> = ordered
        .iter()
        .filter(|p| p.staleness.is_some_and(|s| s.bucket == StalenessBucket::Fresh))
        .take(NOW_SLOTS)
        .map(|p| &p.project.path)
        .collect();
    ordered
        .iter()
        .map(|p| {
            let column = if now_paths.contains(&&p.project.path) {
                BoardColumn::Now
            } else {
                match p.staleness.map(|s| s.bucket) {
                    None | Some(StalenessBucket::Fresh | StalenessBucket::Cooling) => BoardColumn::Next,
                    Some(StalenessBucket::Stale) => BoardColumn::Paused,
                    Some(StalenessBucket::Dormant) => match p.mess.map(|m| m.label) {
                        Some(MessLabel::Clean) => BoardColumn::Maintenance,
                        _ => BoardColumn::Archived,
                    },
                }
            };
            (p.project.path.clone(), column)
        })
        .collect()
}

fn sessions_7d(p: &ProjectReport) -> usize {
    p.activity.as_ref().map_or(0, |a| a.sessions_7d)
}

/// Place every project of `projects` per [`suggest`] (projects must already be in the store, e.g.
/// after `scan::persist`). Returns how many cards were placed.
pub fn apply_suggested(
    store: &mut Store,
    projects: &[ProjectReport],
    now: DateTime<Utc>,
) -> rusqlite::Result<usize> {
    let rows = store.projects()?;
    let placement: Vec<(i64, BoardColumn)> = suggest(projects)
        .into_iter()
        .filter_map(|(path, col)| rows.iter().find(|r| r.path == path).map(|r| (r.id, col)))
        .collect();
    store.place_all(&placement, now)?;
    Ok(placement.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{DiscoveredProject, Markers};
    use crate::mtime::MtimeScan;
    use crate::score::{Mess, Staleness};
    use crate::sessions::{Agent, ProjectActivity};

    fn project(name: &str, days: Option<f64>, mess: Option<MessLabel>, sessions_7d: usize) -> ProjectReport {
        let now = Utc::now();
        ProjectReport {
            project: DiscoveredProject {
                path: PathBuf::from(format!("/p/{name}")),
                title: name.into(),
                root: "/p".into(),
                markers: Markers::default(),
                sub_repos: vec![],
                alias: None,
                from_logs: false,
            },
            repo: None,
            sub_repos: vec![],
            mtime: MtimeScan {
                has_vcs: mess.is_some(),
                latest: None,
                files_seen: 0,
            },
            activity: (sessions_7d > 0).then(|| ProjectActivity {
                last_session_at: now,
                last_agent: Agent::Codex,
                agents: vec![Agent::Codex],
                sessions_7d,
                sessions_30d: sessions_7d,
                sessions_total: sessions_7d,
                last_prompts: vec![],
                final_message: None,
                title: None,
                tokens: Default::default(),
            }),
            mess_inputs: None,
            staleness: days.map(|d| Staleness {
                last_activity: now,
                days: d,
                bucket: StalenessBucket::from_days(d),
            }),
            mess: mess.map(|label| Mess { score: 10.0, label }),
        }
    }

    fn columns(s: &[(PathBuf, BoardColumn)]) -> Vec<(String, BoardColumn)> {
        s.iter()
            .map(|(p, c)| (p.file_name().unwrap().to_string_lossy().into_owned(), *c))
            .collect()
    }

    #[test]
    fn heuristic_per_bucket_and_mess() {
        use BoardColumn::*;
        use MessLabel::*;
        let projects = [
            project("fresh-a", Some(0.5), Some(Clean), 0),
            project("fresh-b", Some(1.0), Some(TidyUp), 4),
            project("fresh-c", Some(1.0), Some(Clean), 1), // same day as b, fewer sessions
            project("fresh-d", Some(2.0), None, 0),        // 4th fresh → Next
            project("cooling", Some(5.0), Some(AbandonedMidWay), 0),
            project("stale", Some(30.0), Some(Clean), 0),
            project("dormant-clean", Some(100.0), Some(Clean), 0),
            project("dormant-messy", Some(100.0), Some(TidyUp), 0),
            project("dormant-abandoned", Some(200.0), Some(AbandonedMidWay), 0),
            project("dormant-novcs", Some(90.0), None, 0),
            project("unknown", None, None, 0),
        ];
        assert_eq!(
            columns(&suggest(&projects)),
            [
                ("fresh-a".to_string(), Now),
                ("fresh-b".into(), Now),
                ("fresh-c".into(), Now),
                ("fresh-d".into(), Next),
                ("cooling".into(), Next),
                ("stale".into(), Paused),
                ("dormant-novcs".into(), Archived),
                ("dormant-clean".into(), Maintenance),
                ("dormant-messy".into(), Archived),
                ("dormant-abandoned".into(), Archived),
                ("unknown".into(), Next),
            ]
        );
        // Fewer than 3 fresh projects: Now holds only those.
        let few = [project("x", Some(1.0), None, 0), project("y", Some(5.0), None, 0)];
        assert_eq!(
            columns(&suggest(&few)),
            [("x".to_string(), Now), ("y".into(), Next)]
        );
    }

    #[test]
    fn apply_places_and_orders_cards_and_survives_reopen() {
        use BoardColumn::*;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logpose.sqlite");
        let now = Utc::now();
        let projects = [
            project("old", Some(100.0), Some(MessLabel::Clean), 0),
            project("new", Some(0.2), None, 0),
            project("stale", Some(20.0), None, 0),
        ];
        {
            let mut store = Store::open(&path).unwrap();
            for p in &projects {
                store
                    .upsert_project(&p.project.path, &p.project.title, now)
                    .unwrap();
            }
            // A manual move first; restoring the suggestion overrides it.
            let new_id = store.projects().unwrap()[1].id;
            store.move_before(new_id, Archived, None, now).unwrap();
            assert_eq!(apply_suggested(&mut store, &projects, now).unwrap(), 3);
        }
        let store = Store::open(&path).unwrap();
        let rows = store.projects().unwrap();
        let placed: Vec<(&str, BoardColumn, i64)> = rows
            .iter()
            .map(|r| (r.title.as_str(), r.column, r.position))
            .collect();
        assert_eq!(
            placed,
            [("new", Now, 0), ("stale", Paused, 0), ("old", Maintenance, 0)]
        );
        let old = rows.iter().find(|r| r.title == "old").unwrap();
        assert!(old.maintenance_at.is_some() && old.archived_at.is_none());
        assert!(
            rows.iter()
                .find(|r| r.title == "new")
                .unwrap()
                .archived_at
                .is_none(),
            "left Archived"
        );
    }
}
