//! Opt-in agent rule (LATER-001, BLUEPRINT §5.5): a marked block in a project's `CLAUDE.md` /
//! `AGENTS.md` asking the working agent to keep `NEXT.md` current at the end of each session. When
//! it does, SYN-006 reads `NEXT.md` and skips the synthesis call.
//!
//! The block sits between `BEGIN` and `END` markers, so it is replaced in place (idempotent) and
//! removed cleanly; text outside it is never touched. Writes are atomic.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::sessions::Agent;

pub const BEGIN: &str = "<!-- logpose:rule:v1 -->";
pub const END: &str = "<!-- /logpose:rule -->";

/// The block (without trailing newline).
pub fn block() -> String {
    format!(
        "{BEGIN}\n## Keep NEXT.md current (Logpose)\n\n\
At the end of each working session, update `NEXT.md` in the project root so the next session can resume quickly. \
Keep this exact format and set the first line to the current UTC time and your agent name \
(`claude-code` or `codex`):\n\n\
```markdown\n\
<!-- logpose:v1 updated=YYYY-MM-DDTHH:MM:SSZ by=claude-code -->\n\
# Next\n\n## About\nOne line: what this project is.\n\n## State\nOne short paragraph: where it stands.\n\n\
## Last goal\nWhat this session was trying to do.\n\n## Next steps\n- [ ] Most important next step\n\n\
## Blockers\n- None.\n\
```\n{END}"
    )
}

/// The instruction file each agent reads.
pub fn file_for(agent: Agent) -> &'static str {
    match agent {
        Agent::ClaudeCode => "CLAUDE.md",
        Agent::Codex => "AGENTS.md",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RuleStatus {
    /// Instruction files that contain the rule.
    pub files: Vec<String>,
}

impl RuleStatus {
    pub fn enabled(&self) -> bool {
        !self.files.is_empty()
    }
}

/// Byte range of the block (markers included) in `text`.
fn find_block(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BEGIN)?;
    let end = text[start..].find(END)? + start + END.len();
    Some((start, end))
}

pub fn status(project: &Path) -> RuleStatus {
    RuleStatus {
        files: ["CLAUDE.md", "AGENTS.md"]
            .into_iter()
            .filter(|f| std::fs::read_to_string(project.join(f)).is_ok_and(|t| find_block(&t).is_some()))
            .map(str::to_string)
            .collect(),
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("md.logpose-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Add (or refresh) the rule in every existing `CLAUDE.md` / `AGENTS.md`, and create the file of each
/// agent in `agents` that has none. With no existing file and no agents, `AGENTS.md` is created.
/// Returns the files written.
pub fn enable(project: &Path, agents: &[Agent]) -> std::io::Result<Vec<PathBuf>> {
    let mut targets: Vec<&str> = ["CLAUDE.md", "AGENTS.md"]
        .into_iter()
        .filter(|f| project.join(f).is_file())
        .collect();
    for a in agents {
        let f = file_for(*a);
        if !targets.contains(&f) {
            targets.push(f);
        }
    }
    if targets.is_empty() {
        targets.push("AGENTS.md");
    }
    let mut written = Vec::new();
    for f in targets {
        let path = project.join(f);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let new = match find_block(&text) {
            Some((s, e)) => format!("{}{}{}", &text[..s], block(), &text[e..]),
            None if text.trim().is_empty() => format!("{}\n", block()),
            None => format!("{}\n\n{}\n", text.trim_end(), block()),
        };
        if new != text {
            write_atomic(&path, &new)?;
            written.push(path);
        }
    }
    Ok(written)
}

/// Remove the rule from both files, leaving everything else as it was. A file that only held the
/// rule is deleted only if Logpose created it, i.e. nothing else remains.
pub fn disable(project: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut changed = Vec::new();
    for f in ["CLAUDE.md", "AGENTS.md"] {
        let path = project.join(f);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some((s, e)) = find_block(&text) else { continue };
        let before = text[..s].trim_end();
        let after = text[e..].trim_start_matches('\n');
        let rest = match (before.is_empty(), after.trim().is_empty()) {
            (true, true) => String::new(),
            (true, false) => after.to_string(),
            (false, true) => format!("{before}\n"),
            (false, false) => format!("{before}\n\n{after}"),
        };
        if rest.is_empty() {
            std::fs::remove_file(&path)?;
        } else {
            write_atomic(&path, &rest)?;
        }
        changed.push(path);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(p: &Path, f: &str) -> String {
        std::fs::read_to_string(p.join(f)).unwrap()
    }

    #[test]
    fn enable_is_idempotent_and_keeps_existing_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(p.join("CLAUDE.md"), "# My rules\n\nUse tabs.\n").unwrap();
        let written = enable(p, &[]).unwrap();
        assert_eq!(written, [p.join("CLAUDE.md")], "only the existing file");
        let text = read(p, "CLAUDE.md");
        assert!(text.starts_with("# My rules\n\nUse tabs.\n\n<!-- logpose:rule:v1 -->"));
        assert!(text.contains("<!-- logpose:v1 updated=") && text.ends_with("<!-- /logpose:rule -->\n"));
        assert!(
            enable(p, &[]).unwrap().is_empty(),
            "second enable changes nothing"
        );
        assert_eq!(status(p).files, ["CLAUDE.md"]);

        // An outdated block is replaced in place; text after it is kept.
        let old = text.replace("Keep NEXT.md current (Logpose)", "Old title") + "\nAfter.\n";
        std::fs::write(p.join("CLAUDE.md"), &old).unwrap();
        enable(p, &[]).unwrap();
        let text = read(p, "CLAUDE.md");
        assert!(
            text.contains("Keep NEXT.md current")
                && !text.contains("Old title")
                && text.ends_with("\nAfter.\n")
        );
        assert_eq!(text.matches(BEGIN).count(), 1);
    }

    #[test]
    fn enable_creates_files_for_the_agents_used() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let written = enable(p, &[Agent::Codex, Agent::ClaudeCode]).unwrap();
        assert_eq!(written, [p.join("AGENTS.md"), p.join("CLAUDE.md")]);
        assert!(read(p, "AGENTS.md").starts_with(BEGIN));
        assert_eq!(status(p).files, ["CLAUDE.md", "AGENTS.md"]);

        let none = tempfile::tempdir().unwrap();
        assert_eq!(enable(none.path(), &[]).unwrap(), [none.path().join("AGENTS.md")]);
    }

    #[test]
    fn disable_restores_the_original_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let original = "# My rules\n\nUse tabs.\n";
        std::fs::write(p.join("CLAUDE.md"), original).unwrap();
        enable(p, &[Agent::Codex]).unwrap();
        let changed = disable(p).unwrap();
        assert_eq!(changed.len(), 2);
        assert_eq!(read(p, "CLAUDE.md"), original);
        assert!(
            !p.join("AGENTS.md").exists(),
            "a file that only held the rule is removed"
        );
        assert!(!status(p).enabled());
        assert!(disable(p).unwrap().is_empty());

        // Text on both sides of the block survives.
        std::fs::write(p.join("AGENTS.md"), format!("Before.\n\n{}\n\nAfter.\n", block())).unwrap();
        disable(p).unwrap();
        assert_eq!(read(p, "AGENTS.md"), "Before.\n\nAfter.\n");
    }

    #[test]
    fn the_example_header_parses_as_next_md() {
        let b = block();
        let example = &b[b.find("<!-- logpose:v1").unwrap()..b.find("```\n<!-- /logpose").unwrap()];
        let filled = example.replace("YYYY-MM-DDTHH:MM:SSZ", "2026-11-02T10:00:00Z");
        let doc = crate::next_md::parse(&filled);
        assert_eq!(doc.header.unwrap().by, "claude-code");
        assert_eq!(doc.next_steps.len(), 1);
    }
}
