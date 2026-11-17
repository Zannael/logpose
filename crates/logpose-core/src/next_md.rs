//! `NEXT.md` convention (BLUEPRINT §8, SYN-007): render, write, and parse.
//!
//! Per DEC-001 the file is committed and shared, so Logpose only writes `<project>/NEXT.md` and never
//! touches `.gitignore`. A `NEXT.md` without the Logpose header was written by a person and is never
//! overwritten.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use crate::synthesis::{Confidence, Synthesis};

pub const FILE_NAME: &str = "NEXT.md";
const HEADER_PREFIX: &str = "<!-- logpose:v1 ";

#[derive(Debug, thiserror::Error)]
pub enum NextMdError {
    #[error("{0} exists and was not written by Logpose; refusing to overwrite it")]
    NotOwned(PathBuf),
    #[error("cannot write {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Header {
    pub updated: DateTime<Utc>,
    /// Who wrote it: `logpose`, `claude-code`, `codex`, …
    pub by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub done: bool,
    pub text: String,
}

/// A parsed `NEXT.md`. Missing sections are empty; unknown sections are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NextMd {
    pub header: Option<Header>,
    pub about: String,
    pub state: String,
    pub last_goal: String,
    pub next_steps: Vec<Step>,
    pub blockers: Vec<String>,
}

/// Render a synthesis in the v1 format (BLUEPRINT §8 plus `About` and `Last goal` sections).
pub fn render(s: &Synthesis, updated: DateTime<Utc>, by: &str) -> String {
    let mut out = format!(
        "{HEADER_PREFIX}updated={} by={by} -->\n# Next\n\n## About\n{}\n\n## State\n{}\n\n## Last goal\n{}\n\n## Next steps\n",
        updated.to_rfc3339_opts(SecondsFormat::Secs, true),
        s.description,
        s.state,
        if s.last_goal.is_empty() {
            "Unknown."
        } else {
            &s.last_goal
        },
    );
    for step in &s.next_steps {
        out.push_str(&format!("- [ ] {step}\n"));
    }
    out.push_str("\n## Blockers\n");
    if s.blockers.is_empty() {
        out.push_str("- None.\n");
    }
    for b in &s.blockers {
        out.push_str(&format!("- {b}\n"));
    }
    out
}

pub fn parse_header(text: &str) -> Option<Header> {
    let line = text.lines().next()?.trim();
    let body = line.strip_prefix(HEADER_PREFIX)?.strip_suffix("-->")?;
    let mut updated = None;
    let mut by = None;
    for pair in body.split_whitespace() {
        match pair.split_once('=') {
            Some(("updated", v)) => {
                updated = DateTime::parse_from_rfc3339(v)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            }
            Some(("by", v)) => by = Some(v.to_string()),
            _ => {}
        }
    }
    Some(Header {
        updated: updated?,
        by: by.unwrap_or_default(),
    })
}

pub fn parse(text: &str) -> NextMd {
    let mut doc = NextMd {
        header: parse_header(text),
        ..NextMd::default()
    };
    let mut section = String::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let flush = |doc: &mut NextMd, section: &str, paragraph: &mut Vec<&str>| {
        let text = paragraph.join(" ").trim().to_string();
        paragraph.clear();
        match section {
            "about" => doc.about = text,
            "state" => doc.state = text,
            "last goal" => doc.last_goal = text,
            _ => {}
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("## ") {
            flush(&mut doc, &section, &mut paragraph);
            section = title.trim().to_lowercase();
            continue;
        }
        match section.as_str() {
            "next steps" => {
                if let Some(step) = parse_step(trimmed) {
                    doc.next_steps.push(step);
                }
            }
            "blockers" => {
                if let Some(item) = trimmed.strip_prefix("- ").map(str::trim)
                    && !item.is_empty()
                    && !item.eq_ignore_ascii_case("none.")
                    && !item.eq_ignore_ascii_case("none")
                {
                    doc.blockers.push(item.to_string());
                }
            }
            "about" | "state" | "last goal" if !trimmed.is_empty() => paragraph.push(trimmed),
            _ => {}
        }
    }
    flush(&mut doc, &section, &mut paragraph);
    doc
}

fn parse_step(line: &str) -> Option<Step> {
    let rest = line.strip_prefix("- ")?;
    let (done, text) = if let Some(t) = rest.strip_prefix("[ ] ") {
        (false, t)
    } else if let Some(t) = rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")) {
        (true, t)
    } else {
        (false, rest)
    };
    let text = text.trim();
    (!text.is_empty()).then(|| Step {
        done,
        text: text.to_string(),
    })
}

impl NextMd {
    /// SYN-006: a file with the Logpose header written at or after the project's latest activity
    /// (commit, file change or agent session) still describes the project, so synthesis can be
    /// skipped. Headerless files have no date and never count as current.
    pub fn is_current(&self, last_activity: Option<DateTime<Utc>>) -> bool {
        match (&self.header, last_activity) {
            // The header has whole seconds, so compare at that precision.
            (Some(h), Some(last)) => h.updated.timestamp() >= last.timestamp(),
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// The file as a synthesis: open steps only, `Unknown.` goal as empty. The confidence is not
    /// stored in `NEXT.md` and the file may have been edited by hand, so it is `Medium`.
    pub fn to_synthesis(&self) -> Synthesis {
        Synthesis {
            description: self.about.clone(),
            state: self.state.clone(),
            last_goal: if self.last_goal.eq_ignore_ascii_case("unknown.") {
                String::new()
            } else {
                self.last_goal.clone()
            },
            next_steps: self
                .next_steps
                .iter()
                .filter(|s| !s.done)
                .map(|s| s.text.clone())
                .collect(),
            blockers: self.blockers.clone(),
            confidence: Confidence::Medium,
        }
    }
}

/// `<project>/NEXT.md` when it is current for `last_activity` ([`NextMd::is_current`]).
pub fn read_current(project: &Path, last_activity: Option<DateTime<Utc>>) -> Option<NextMd> {
    read(project).filter(|m| m.is_current(last_activity))
}

/// Read `<project>/NEXT.md`; `None` when absent or unreadable.
pub fn read(project: &Path) -> Option<NextMd> {
    std::fs::read_to_string(project.join(FILE_NAME))
        .ok()
        .map(|t| parse(&t))
}

/// Write `<project>/NEXT.md` atomically. Refuses to replace a file without the Logpose header.
pub fn write(
    project: &Path,
    s: &Synthesis,
    updated: DateTime<Utc>,
    by: &str,
) -> Result<PathBuf, NextMdError> {
    let path = project.join(FILE_NAME);
    if let Ok(existing) = std::fs::read_to_string(&path)
        && parse_header(&existing).is_none()
    {
        return Err(NextMdError::NotOwned(path));
    }
    let io = |source| NextMdError::Io {
        path: path.clone(),
        source,
    };
    let mut tmp = tempfile::NamedTempFile::new_in(project).map_err(io)?;
    tmp.write_all(render(s, updated, by).as_bytes()).map_err(io)?;
    tmp.persist(&path).map_err(|e| io(e.error))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthesis::Confidence;

    fn synthesis() -> Synthesis {
        Synthesis {
            description: "Telegram deals bot".into(),
            state: "Alerts work; tests missing.".into(),
            last_goal: "Add price alerts".into(),
            next_steps: vec!["Add threshold tests".into(), "Deploy".into()],
            blockers: vec![],
            confidence: Confidence::High,
        }
    }

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-31T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn render_matches_convention() {
        let text = render(&synthesis(), at(), "logpose");
        assert!(text.starts_with("<!-- logpose:v1 updated=2026-10-31T10:00:00Z by=logpose -->\n# Next\n"));
        assert!(text.contains("## Next steps\n- [ ] Add threshold tests\n- [ ] Deploy\n"));
        assert!(text.ends_with("## Blockers\n- None.\n"));
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), &synthesis(), at(), "logpose").unwrap();
        assert_eq!(path, dir.path().join("NEXT.md"));
        let doc = read(dir.path()).unwrap();
        assert_eq!(
            doc.header,
            Some(Header {
                updated: at(),
                by: "logpose".into()
            })
        );
        assert_eq!(doc.about, "Telegram deals bot");
        assert_eq!(doc.state, "Alerts work; tests missing.");
        assert_eq!(doc.last_goal, "Add price alerts");
        let steps: Vec<_> = doc.next_steps.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(steps, ["Add threshold tests", "Deploy"]);
        assert!(doc.blockers.is_empty());
        // Overwriting our own file is allowed.
        write(dir.path(), &synthesis(), at(), "logpose").unwrap();
    }

    #[test]
    fn freshness_against_latest_activity() {
        let dir = tempfile::tempdir().unwrap();
        let hour = chrono::Duration::hours(1);
        // Missing.
        assert!(read_current(dir.path(), Some(at())).is_none());
        // Fresh: written after the latest activity (and exactly at it).
        write(dir.path(), &synthesis(), at(), "logpose").unwrap();
        assert!(read_current(dir.path(), Some(at() - hour)).is_some());
        assert!(read_current(dir.path(), Some(at())).is_some());
        assert!(read_current(dir.path(), None).is_some(), "no known activity");
        // Same second as the activity (the header drops sub-seconds).
        assert!(read_current(dir.path(), Some(at() + chrono::Duration::milliseconds(700))).is_some());
        // Stale: activity after the file was written.
        assert!(read_current(dir.path(), Some(at() + hour)).is_none());
        // Headerless: written by a person, no date.
        std::fs::write(dir.path().join(FILE_NAME), "# Next\n\n## State\nMine.\n").unwrap();
        assert!(read(dir.path()).is_some());
        assert!(read_current(dir.path(), Some(at() - hour)).is_none());
        assert!(read_current(dir.path(), None).is_none());
    }

    #[test]
    fn current_file_becomes_a_synthesis() {
        let mut s = synthesis();
        s.last_goal.clear();
        let text = render(&s, at(), "codex").replace("- [ ] Deploy", "- [x] Deploy");
        let back = parse(&text).to_synthesis();
        assert_eq!(back.description, "Telegram deals bot");
        assert_eq!(back.last_goal, "", "Unknown. maps back to empty");
        assert_eq!(back.next_steps, ["Add threshold tests"], "done steps dropped");
        assert!(back.blockers.is_empty());
        assert_eq!(back.confidence, Confidence::Medium);
    }

    #[test]
    fn never_overwrites_a_human_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("NEXT.md"), "# My own plan\n").unwrap();
        assert!(matches!(
            write(dir.path(), &synthesis(), at(), "logpose"),
            Err(NextMdError::NotOwned(_))
        ));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("NEXT.md")).unwrap(),
            "# My own plan\n"
        );
    }

    #[test]
    fn parses_agent_edits_and_headerless_files() {
        let doc = parse(
            "<!-- logpose:v1 updated=2026-11-01T08:30:00+01:00 by=claude-code -->\n# Next\n\n## State\nLine one\nline two.\n\n\
             ## Next steps\n- [x] Done thing\n- [ ] Open thing\n- plain item\n\n## Blockers\n- Waiting on API key\n\n## Extra\nignored\n",
        );
        let h = doc.header.unwrap();
        assert_eq!(
            (h.by.as_str(), h.updated.to_rfc3339()),
            ("claude-code", "2026-11-01T07:30:00+00:00".into())
        );
        assert_eq!(doc.state, "Line one line two.");
        assert_eq!(
            doc.next_steps[0],
            Step {
                done: true,
                text: "Done thing".into()
            }
        );
        assert_eq!(doc.next_steps.len(), 3);
        assert_eq!(doc.blockers, ["Waiting on API key"]);
        assert_eq!(parse("# Next\n## State\nx\n").header, None);
        assert_eq!(parse_header("<!-- logpose:v1 by=x -->"), None);
    }
}
