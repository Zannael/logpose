//! Context Packer (BLUEPRINT §5.4, SYN-001): a compact, pre-digested text about one project, so the
//! agent never explores the repo. Zero tokens to build.
//!
//! The pack is a list of sections. The header (identity + signals) is always kept; the other sections
//! are shrunk in priority order (least useful first) until the estimated size fits the budget.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::git;
use crate::next_md;
use crate::scan::ProjectReport;

/// Rough token estimate: ~4 characters per token (heuristic, not a tokenizer).
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

pub const EXCERPT_MAX_LINES: usize = 60;
pub const COMMITS: usize = 10;
pub const MAX_FILES: usize = 40;

#[derive(Debug, Clone)]
pub struct PackOptions {
    pub budget_tokens: usize,
    /// Include recent prompts and the last agent message (per-project privacy opt-out, SYN-008).
    pub include_conversation: bool,
    pub parking_note: Option<String>,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            budget_tokens: 3000,
            include_conversation: true,
            parking_note: None,
        }
    }
}

impl PackOptions {
    /// Options for a stored project: its parking note and privacy opt-out (SYN-008).
    pub fn for_project(row: &crate::store::ProjectRow, budget_tokens: usize) -> Self {
        Self {
            budget_tokens,
            include_conversation: !row.private,
            parking_note: row.parking_note.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    title: &'static str,
    lines: Vec<String>,
    /// Lower = dropped first.
    priority: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub text: String,
    pub estimated_tokens: usize,
    /// Section titles that were shortened or dropped to fit the budget, in order.
    pub truncated: Vec<&'static str>,
}

fn relative(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let d = now - t;
    match d.num_days() {
        0 if d.num_hours() < 1 => "just now".into(),
        0 => format!("{}h ago", d.num_hours()),
        1..=59 => format!("{}d ago", d.num_days()),
        n => format!("{}mo ago", n / 30),
    }
}

/// First paragraphs of CLAUDE.md / AGENTS.md / README.md (first file found), up to `EXCERPT_MAX_LINES`.
fn excerpt(project: &Path) -> Vec<String> {
    for name in ["CLAUDE.md", "AGENTS.md", "README.md", "README", "readme.md"] {
        let Ok(text) = std::fs::read_to_string(project.join(name)) else {
            continue;
        };
        let mut lines = vec![format!("({name})")];
        let mut paragraphs = 0;
        let mut in_text = false;
        for line in text.lines().skip_while(|l| l.trim().is_empty()) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if in_text {
                    paragraphs += 1;
                    in_text = false;
                    if paragraphs == 2 {
                        break;
                    }
                }
                continue;
            }
            in_text |= !trimmed.starts_with('#');
            lines.push(trimmed.to_string());
            if lines.len() > EXCERPT_MAX_LINES {
                break;
            }
        }
        return lines;
    }
    Vec::new()
}

fn signals_line(r: &ProjectReport) -> String {
    let mut kv: Vec<String> = Vec::new();
    if let Some(s) = r.staleness {
        kv.push(format!("staleness_days={:.0}", s.days));
        kv.push(format!("bucket={:?}", s.bucket).to_lowercase());
    }
    if let Some(m) = r.mess {
        kv.push(format!("mess={:.0}", m.score));
        kv.push(format!("mess_label={:?}", m.label).to_lowercase());
    }
    if let Some(sig) = r.repo.as_ref().and_then(|g| g.signals.as_ref()) {
        kv.push(format!("branch={}", sig.branch.as_deref().unwrap_or("detached")));
        if let Some(d) = &sig.default_branch {
            kv.push(format!("default_branch={d}"));
        }
        kv.push(format!("uncommitted_files={}", sig.uncommitted_files()));
        kv.push(format!("ahead={} behind={}", sig.ahead, sig.behind));
        if sig.no_upstream() {
            kv.push("no_upstream=true".into());
        }
        if sig.stash_count > 0 {
            kv.push(format!("stashes={}", sig.stash_count));
        }
        if let Some(op) = sig.operation {
            kv.push(format!("in_progress={op:?}").to_lowercase());
        }
    } else {
        kv.push("vcs=none".into());
    }
    if let Some(i) = &r.mess_inputs {
        if let Some(d) = i.oldest_uncommitted_days {
            kv.push(format!("oldest_uncommitted_days={d:.0}"));
        }
        if i.stale_branches > 0 {
            kv.push(format!("unmerged_branches={}", i.stale_branches));
        }
    }
    if !r.sub_repos.is_empty() {
        kv.push(format!("sub_repos={}", r.sub_repos.len()));
    }
    if let Some(a) = &r.activity {
        let agents: Vec<String> = a.agents.iter().map(|x| format!("{x:?}").to_lowercase()).collect();
        kv.push(format!("agents={}", agents.join(",")));
        kv.push(format!("sessions_30d={}", a.sessions_30d));
    }
    kv.join(" ")
}

fn cap_line(s: &str, max: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::sessions::truncate_chars(&one_line, max)
}

/// Build the pack for one scanned project.
pub fn pack(r: &ProjectReport, opts: &PackOptions, now: DateTime<Utc>) -> Pack {
    let path = &r.project.path;
    let mut header = vec![format!(
        "Project: {} ({})",
        r.project.display_name(),
        path.display()
    )];
    header.push(format!("Signals: {}", signals_line(r)));
    if let Some(note) = opts.parking_note.as_deref().filter(|n| !n.trim().is_empty()) {
        header.push(format!("Parking note from the user: {}", cap_line(note, 300)));
    }

    let mut sections = Vec::new();
    let about = excerpt(path);
    if !about.is_empty() {
        sections.push(Section {
            title: "About",
            lines: about,
            priority: 4,
        });
    }
    if let Some(doc) = next_md::read(path) {
        let mut lines = Vec::new();
        if !doc.state.is_empty() {
            lines.push(format!("State: {}", cap_line(&doc.state, 600)));
        }
        lines.extend(doc.next_steps.iter().map(|s| {
            format!(
                "- [{}] {}",
                if s.done { "x" } else { " " },
                cap_line(&s.text, 200)
            )
        }));
        lines.extend(
            doc.blockers
                .iter()
                .map(|b| format!("Blocker: {}", cap_line(b, 200))),
        );
        if let Some(h) = &doc.header {
            lines.insert(0, format!("(written {} by {})", relative(h.updated, now), h.by));
        }
        sections.push(Section {
            title: "Current NEXT.md",
            lines,
            priority: 8,
        });
    }
    if r.repo.is_some() {
        if let Ok(commits) = git::recent_commits(path, COMMITS) {
            let lines: Vec<String> = commits
                .iter()
                .map(|(t, s)| format!("- {}: {}", relative(*t, now), cap_line(s, 120)))
                .collect();
            if !lines.is_empty() {
                sections.push(Section {
                    title: "Recent commits",
                    lines,
                    priority: 6,
                });
            }
        }
        if let Ok((tracked, untracked)) = git::changed_paths(path) {
            let take = |v: Vec<String>| {
                let extra = v.len().saturating_sub(MAX_FILES);
                let mut lines: Vec<String> = v.into_iter().take(MAX_FILES).collect();
                if extra > 0 {
                    lines.push(format!("… and {extra} more"));
                }
                lines
            };
            if !tracked.is_empty() {
                sections.push(Section {
                    title: "Uncommitted changes (files)",
                    lines: take(tracked),
                    priority: 3,
                });
            }
            if !untracked.is_empty() {
                sections.push(Section {
                    title: "Untracked files",
                    lines: take(untracked),
                    priority: 1,
                });
            }
        }
    }
    if opts.include_conversation
        && let Some(a) = &r.activity
    {
        let prompts: Vec<String> = a
            .last_prompts
            .iter()
            .map(|p| format!("- {}", cap_line(&p.text, 400)))
            .collect();
        if !prompts.is_empty() {
            sections.push(Section {
                title: "Last user prompts to coding agents (oldest first)",
                lines: prompts,
                priority: 7,
            });
        }
        if let Some(m) = &a.final_message {
            sections.push(Section {
                title: "Last agent message",
                lines: vec![cap_line(m, 800)],
                priority: 5,
            });
        }
    }

    let render = |sections: &[Section]| {
        let mut out = header.join("\n");
        for s in sections.iter().filter(|s| !s.lines.is_empty()) {
            out.push_str(&format!("\n\n## {}\n{}", s.title, s.lines.join("\n")));
        }
        out
    };
    let mut truncated = Vec::new();
    let mut text = render(&sections);
    while estimate_tokens(&text) > opts.budget_tokens {
        // Shrink the lowest-priority non-empty section: halve its lines, then drop it.
        let Some(victim) = sections
            .iter_mut()
            .filter(|s| !s.lines.is_empty())
            .min_by_key(|s| s.priority)
        else {
            break;
        };
        let keep = if victim.lines.len() > 1 {
            victim.lines.len() / 2
        } else {
            0
        };
        victim.lines.truncate(keep);
        if truncated.last() != Some(&victim.title) {
            truncated.push(victim.title);
        }
        text = render(&sections);
    }
    Pack {
        estimated_tokens: estimate_tokens(&text),
        text,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::git::testutil::{git, repo_with_upstream, write};
    use crate::scan::{self, LogSources};

    fn report(root: &Path) -> ProjectReport {
        let config = Config {
            roots: vec![root.to_owned()],
            ..Config::default()
        };
        let mut r = scan::scan(&config, &LogSources::default(), Utc::now()).unwrap();
        r.projects.remove(0)
    }

    fn project_with_many_files() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        write(
            &w,
            "README.md",
            "# Demo\n\nA Telegram bot that tracks deals.\n\nSecond paragraph.\n\nThird paragraph (excluded).\n",
        );
        git(&w, &["add", "README.md"]);
        git(&w, &["commit", "-qm", "add readme"]);
        write(&w, "f.txt", "changed\n");
        for i in 0..80 {
            write(
                &w,
                &format!("untracked_with_a_rather_long_file_name_{i:03}.txt"),
                "u",
            );
        }
        (dir, w)
    }

    #[test]
    fn full_pack_contents() {
        let (dir, w) = project_with_many_files();
        let r = report(dir.path());
        assert_eq!(r.project.path, w);
        let p = pack(
            &r,
            &PackOptions {
                parking_note: Some("waiting on API key".into()),
                ..PackOptions::default()
            },
            Utc::now(),
        );
        assert!(p.text.starts_with("Project: work ("));
        assert!(p.text.contains("Signals: staleness_days=0 bucket=fresh"));
        assert!(p.text.contains("Parking note from the user: waiting on API key"));
        assert!(
            p.text.contains(
                "## About\n(README.md)\n# Demo\nA Telegram bot that tracks deals.\nSecond paragraph."
            )
        );
        assert!(!p.text.contains("Third paragraph"));
        assert!(p.text.contains("- just now: add readme"));
        assert!(p.text.contains("## Uncommitted changes (files)\nf.txt"));
        assert!(p.text.contains("… and 40 more"), "file lists are capped");
        assert!(p.estimated_tokens <= 3000);
    }

    #[test]
    fn truncates_in_priority_order_and_respects_budget() {
        let (dir, _) = project_with_many_files();
        let r = report(dir.path());
        let full = pack(&r, &PackOptions::default(), Utc::now());
        assert!(full.truncated.is_empty());
        for budget in [400, 250, 120] {
            let p = pack(
                &r,
                &PackOptions {
                    budget_tokens: budget,
                    ..PackOptions::default()
                },
                Utc::now(),
            );
            assert!(
                p.estimated_tokens <= budget,
                "budget {budget}: {}",
                p.estimated_tokens
            );
            assert_eq!(
                p.truncated.first(),
                Some(&"Untracked files"),
                "lowest priority goes first"
            );
            assert!(p.text.starts_with("Project: work"), "header always kept");
        }
        let tight = pack(
            &r,
            &PackOptions {
                budget_tokens: 120,
                ..PackOptions::default()
            },
            Utc::now(),
        );
        assert!(tight.estimated_tokens <= 120);
        let very_tight = pack(
            &r,
            &PackOptions {
                budget_tokens: 40,
                ..PackOptions::default()
            },
            Utc::now(),
        );
        // Sections are shrunk strictly in ascending priority.
        let priority = [
            "Untracked files",
            "Uncommitted changes (files)",
            "About",
            "Recent commits",
        ];
        let ranks: Vec<usize> = very_tight
            .truncated
            .iter()
            .map(|t| priority.iter().position(|p| p == t).unwrap())
            .collect();
        assert!(ranks.len() >= 3, "{:?}", very_tight.truncated);
        assert!(
            ranks.windows(2).all(|w| w[0] < w[1]),
            "{:?}",
            very_tight.truncated
        );
    }

    #[test]
    fn conversation_opt_out_and_next_md() {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        write(
            &w,
            "NEXT.md",
            "<!-- logpose:v1 updated=2026-10-31T10:00:00Z by=codex -->\n# Next\n\n## State\nHalf done.\n\n## Next steps\n- [ ] Finish parser\n",
        );
        let mut r = report(dir.path());
        r.activity = Some(crate::sessions::ProjectActivity {
            last_session_at: Utc::now(),
            last_agent: crate::sessions::Agent::Codex,
            agents: vec![crate::sessions::Agent::Codex],
            sessions_7d: 1,
            sessions_30d: 1,
            sessions_total: 1,
            last_prompts: vec![crate::sessions::Prompt {
                at: None,
                text: "secret prompt".into(),
            }],
            final_message: Some("secret answer".into()),
            title: None,
            tokens: Default::default(),
        });
        let with = pack(&r, &PackOptions::default(), Utc::now());
        assert!(with.text.contains("- secret prompt") && with.text.contains("secret answer"));
        assert!(
            with.text.contains("## Current NEXT.md\n(written") && with.text.contains("- [ ] Finish parser")
        );
        let without = pack(
            &r,
            &PackOptions {
                include_conversation: false,
                ..PackOptions::default()
            },
            Utc::now(),
        );
        assert!(!without.text.contains("secret"));
    }

    #[test]
    fn opted_out_project_pack_has_no_prompt_text() {
        let dir = tempfile::tempdir().unwrap();
        repo_with_upstream(dir.path());
        let mut r = report(dir.path());
        r.activity = Some(crate::sessions::ProjectActivity {
            last_session_at: Utc::now(),
            last_agent: crate::sessions::Agent::ClaudeCode,
            agents: vec![crate::sessions::Agent::ClaudeCode],
            sessions_7d: 1,
            sessions_30d: 1,
            sessions_total: 1,
            last_prompts: vec![crate::sessions::Prompt {
                at: None,
                text: "my private prompt".into(),
            }],
            final_message: Some("private agent reply".into()),
            title: None,
            tokens: Default::default(),
        });
        let store = crate::store::Store::open_in_memory().unwrap();
        let id = store
            .upsert_project(&r.project.path, &r.project.title, Utc::now())
            .unwrap();
        store.set_parking_note(id, Some("back after exams")).unwrap();
        let row = |store: &crate::store::Store| store.project(id).unwrap().unwrap();

        let open = pack(&r, &PackOptions::for_project(&row(&store), 3000), Utc::now());
        assert!(open.text.contains("my private prompt") && open.text.contains("private agent reply"));

        store.set_private(id, true).unwrap();
        let private = pack(&r, &PackOptions::for_project(&row(&store), 3000), Utc::now());
        assert!(!private.text.contains("private prompt") && !private.text.contains("agent reply"));
        assert!(
            private.text.contains("back after exams"),
            "the parking note is still included"
        );
    }

    #[test]
    fn token_estimate_and_relative_dates() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcde"), 2);
        let now = Utc::now();
        assert_eq!(relative(now - chrono::Duration::days(2), now), "2d ago");
        assert_eq!(relative(now - chrono::Duration::days(90), now), "3mo ago");
        assert_eq!(relative(now - chrono::Duration::hours(5), now), "5h ago");
    }
}
