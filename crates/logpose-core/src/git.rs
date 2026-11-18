//! Git Scanner, basic signals (BLUEPRINT §5.2, GIT-001). Uses gix; APIs verified in SPIKE-006.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::mtime::OWN_FILES;

#[derive(Debug, thiserror::Error)]
#[error("git error in {path}: {message}")]
pub struct GitError {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RepoSignals {
    /// Worktree entries the status reported without a summary (not counted above); with
    /// [`Self::uncommitted_files`] at 0 they tell whether [`dirty_stats`] can be skipped (PERF-001).
    #[serde(skip)]
    pub unsummarized: usize,
    /// `None` for a repository without commits.
    pub last_commit_at: Option<DateTime<Utc>>,
    /// `None` when HEAD is detached.
    pub branch: Option<String>,
    pub default_branch: Option<String>,
    pub on_default_branch: bool,
    pub staged: usize,
    pub modified: usize,
    pub untracked: usize,
    pub conflicted: usize,
    /// Upstream tracking ref, e.g. `refs/remotes/origin/main`.
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    /// HEAD commit id (hex), `None` without commits.
    pub head_id: Option<String>,
    pub stash_count: usize,
    pub oldest_stash_at: Option<DateTime<Utc>>,
    /// Merge, rebase, cherry-pick, … in progress (GIT-003).
    pub operation: Option<Operation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Operation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    ApplyMailbox,
}

impl Operation {
    fn from_gix(state: gix::state::InProgress) -> Self {
        use gix::state::InProgress as P;
        match state {
            P::Merge => Self::Merge,
            P::Rebase | P::RebaseInteractive | P::ApplyMailboxRebase => Self::Rebase,
            P::CherryPick | P::CherryPickSequence => Self::CherryPick,
            P::Revert | P::RevertSequence => Self::Revert,
            P::Bisect => Self::Bisect,
            P::ApplyMailbox => Self::ApplyMailbox,
        }
    }
}

/// A local branch not merged into the default branch (GIT-004).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnmergedBranch {
    pub name: String,
    pub tip_at: Option<DateTime<Utc>>,
}

impl RepoSignals {
    pub fn no_upstream(&self) -> bool {
        self.upstream.is_none()
    }

    pub fn uncommitted_files(&self) -> usize {
        self.staged + self.modified + self.untracked + self.conflicted
    }
}

fn err(path: &Path, e: impl std::fmt::Display) -> GitError {
    GitError {
        path: path.display().to_string(),
        message: e.to_string(),
    }
}

fn to_utc(seconds: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(seconds, 0)
}

fn is_own_file(path: &[u8]) -> bool {
    OWN_FILES.iter().any(|f| path == f.as_bytes())
}

pub fn scan_repo(path: &Path) -> Result<RepoSignals, GitError> {
    scan_repo_inner(path, false).map(|(s, _)| s)
}

/// [`scan_repo`] and [`dirty_stats`] from one status pass (PERF-001); a clean repo skips the diffs.
pub fn scan_repo_with_dirty(path: &Path) -> Result<(RepoSignals, DirtyStats), GitError> {
    scan_repo_inner(path, true).map(|(s, d)| (s, d.unwrap_or_default()))
}

fn status_items(repo: &gix::Repository, path: &Path) -> Result<Vec<gix::status::Item>, GitError> {
    Ok(repo
        .status(gix::progress::Discard)
        .map_err(|e| err(path, e))?
        .into_iter(None)
        .map_err(|e| err(path, e))?
        .filter_map(Result::ok)
        .collect())
}

fn scan_repo_inner(path: &Path, want_dirty: bool) -> Result<(RepoSignals, Option<DirtyStats>), GitError> {
    let repo = gix::open(path).map_err(|e| err(path, e))?;
    let mut s = RepoSignals::default();

    let head_commit = repo.head_commit().ok();
    if let Some(c) = &head_commit {
        s.head_id = Some(c.id.to_string());
        s.last_commit_at = c.time().ok().and_then(|t| to_utc(t.seconds));
    }
    let head_name = repo.head_name().map_err(|e| err(path, e))?;
    s.branch = head_name.as_ref().map(|n| n.shorten().to_string());
    s.default_branch = default_branch(&repo);
    s.on_default_branch = s.branch.is_some() && s.branch == s.default_branch;

    let items = status_items(&repo, path)?;
    for item in &items {
        match item {
            gix::status::Item::TreeIndex(change) => {
                if !is_own_file(change.location()) {
                    s.staged += 1;
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                if is_own_file(iw.rela_path()) {
                    continue;
                }
                use gix::status::index_worktree::iter::Summary;
                match iw.summary() {
                    Some(Summary::Added) => s.untracked += 1,
                    Some(Summary::Conflict) => s.conflicted += 1,
                    Some(_) => s.modified += 1,
                    None => s.unsummarized += 1,
                }
            }
        }
    }

    if let (Some(name), Some(head)) = (&head_name, &head_commit)
        && let Some(Ok(upstream)) =
            repo.branch_remote_tracking_ref_name(name.as_ref(), gix::remote::Direction::Fetch)
        && let Ok(Some(mut r)) = repo.try_find_reference(upstream.as_ref())
        && let Ok(up_id) = r.peel_to_id()
    {
        let up_id = up_id.detach();
        s.upstream = Some(upstream.as_bstr().to_string());
        let count = |from: gix::ObjectId, hide: gix::ObjectId| {
            repo.rev_walk([from])
                .with_hidden([hide])
                .all()
                .map(|w| w.count())
                .unwrap_or(0)
        };
        s.ahead = count(head.id, up_id);
        s.behind = count(up_id, head.id);
    }
    s.operation = repo.state().map(Operation::from_gix);
    if let Ok(Some(stash)) = repo.try_find_reference("refs/stash") {
        let mut log = stash.log_iter();
        if let Ok(Some(lines)) = log.all() {
            let times: Vec<i64> = lines.flatten().map(|l| l.signature.seconds()).collect();
            s.stash_count = times.len();
            s.oldest_stash_at = times.iter().min().and_then(|t| to_utc(*t));
        }
    }
    let dirty = (want_dirty && s.uncommitted_files() + s.unsummarized > 0)
        .then(|| dirty_from_items(&repo, path, &items));
    Ok((s, dirty))
}

/// Size of uncommitted work (GIT-002).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DirtyStats {
    /// Lines added/removed across staged (HEAD→index) and unstaged (index→worktree) changes of text files.
    pub lines_added: usize,
    pub lines_removed: usize,
    /// Oldest modification time among dirty files that still exist (incl. untracked).
    pub oldest_change_at: Option<DateTime<Utc>>,
}

const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

/// `(added, removed)` lines, or `None` for binary or oversized content.
fn line_delta(old: &[u8], new: &[u8]) -> Option<(usize, usize)> {
    let binary = |b: &[u8]| b.len() > MAX_DIFF_BYTES || b[..b.len().min(8000)].contains(&0);
    if binary(old) || binary(new) {
        return None;
    }
    let input = gix::diff::blob::InternedInput::new(old, new);
    let diff = gix::diff::blob::diff_with_slider_heuristics(gix::diff::blob::Algorithm::Histogram, &input);
    Some((diff.count_additions() as usize, diff.count_removals() as usize))
}

pub fn dirty_stats(path: &Path) -> Result<DirtyStats, GitError> {
    let repo = gix::open(path).map_err(|e| err(path, e))?;
    let items = status_items(&repo, path)?;
    Ok(dirty_from_items(&repo, path, &items))
}

fn dirty_from_items(repo: &gix::Repository, path: &Path, items: &[gix::status::Item]) -> DirtyStats {
    let workdir = repo
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.to_path_buf());
    // Oversized content gives no line delta anyway, so it is not read at all (PERF-001: a 2.4 GB
    // model file took a second): blobs are size-checked by header, worktree files by metadata.
    // `None` = oversized; either side oversized gives no delta, as in `line_delta`.
    let blob = |id: &gix::oid| -> Option<Vec<u8>> {
        if repo
            .find_header(id)
            .is_ok_and(|h| h.size() as usize > MAX_DIFF_BYTES)
        {
            return None;
        }
        Some(repo.find_object(id).map(|o| o.data.clone()).unwrap_or_default())
    };
    let read = |p: &Path| -> Option<Vec<u8>> {
        if std::fs::metadata(p).is_ok_and(|m| m.len() as usize > MAX_DIFF_BYTES) {
            return None;
        }
        Some(std::fs::read(p).unwrap_or_default())
    };
    let delta = |old: Option<Vec<u8>>, new: Option<Vec<u8>>| line_delta(&old?, &new?);
    let mut stats = DirtyStats::default();
    let mut add = |delta: Option<(usize, usize)>| {
        if let Some((a, r)) = delta {
            stats.lines_added += a;
            stats.lines_removed += r;
        }
    };
    let mut dirty_paths: Vec<std::path::PathBuf> = Vec::new();
    for item in items {
        match item {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef as C;
                if is_own_file(change.location()) {
                    continue;
                }
                dirty_paths.push(workdir.join(gix::path::from_bstr(change.location())));
                match &change {
                    C::Addition { id, .. } => add(delta(Some(Vec::new()), blob(id))),
                    C::Deletion { id, .. } => add(delta(blob(id), Some(Vec::new()))),
                    C::Modification { previous_id, id, .. } => add(delta(blob(previous_id), blob(id))),
                    C::Rewrite { source_id, id, .. } => add(delta(blob(source_id), blob(id))),
                }
            }
            gix::status::Item::IndexWorktree(iw) => {
                if is_own_file(iw.rela_path()) {
                    continue;
                }
                let file = workdir.join(gix::path::from_bstr(iw.rela_path()));
                if let gix::status::index_worktree::Item::Modification { entry, .. } = &iw {
                    add(delta(blob(&entry.id), read(&file)));
                }
                dirty_paths.push(file);
            }
        }
    }
    stats.oldest_change_at = dirty_paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .min()
        .map(DateTime::<Utc>::from);
    stats
}

/// Subject line and time of the last `n` first-parent commits on HEAD (newest first).
pub fn recent_commits(path: &Path, n: usize) -> Result<Vec<(DateTime<Utc>, String)>, GitError> {
    let repo = gix::open(path).map_err(|e| err(path, e))?;
    let Ok(head) = repo.head_commit() else {
        return Ok(Vec::new());
    };
    let walk = repo
        .rev_walk([head.id])
        .first_parent_only()
        .all()
        .map_err(|e| err(path, e))?;
    let mut out = Vec::new();
    for info in walk.take(n) {
        let Ok(info) = info else { break };
        let Ok(commit) = info.object() else { continue };
        let when = commit
            .time()
            .ok()
            .and_then(|t| to_utc(t.seconds))
            .unwrap_or_default();
        let subject = commit
            .message()
            .map(|m| m.summary().to_string())
            .unwrap_or_default();
        out.push((when, subject));
    }
    Ok(out)
}

/// The `user.email` this repository commits with (repo, global and system config), lowercased.
pub fn user_email(path: &Path) -> Option<String> {
    let repo = gix::open(path).ok()?;
    let email = repo.config_snapshot().string("user.email")?;
    let email = email.to_string().trim().to_lowercase();
    (!email.is_empty()).then_some(email)
}

/// Commits authored by one of `emails` (lowercase) at or after `since`, reachable from HEAD or any
/// local branch: `(commit id hex, author time)`. Walks newest first and stops at the cutoff (STATS-001).
pub fn authored_commits(
    path: &Path,
    emails: &[String],
    since: DateTime<Utc>,
) -> Result<Vec<(String, DateTime<Utc>, String)>, GitError> {
    let mut repo = gix::open(path).map_err(|e| err(path, e))?;
    repo.object_cache_size_if_unset(4 * 1024 * 1024);
    let mut tips: Vec<gix::ObjectId> = repo.head_id().ok().map(|id| id.detach()).into_iter().collect();
    if let Ok(refs) = repo.references()
        && let Ok(branches) = refs.local_branches()
    {
        for mut r in branches.flatten() {
            if let Ok(id) = r.peel_to_id() {
                tips.push(id.detach());
            }
        }
    }
    tips.sort();
    tips.dedup();
    if tips.is_empty() {
        return Ok(Vec::new());
    }
    let walk = repo
        .rev_walk(tips)
        .sorting(gix::revision::walk::Sorting::ByCommitTimeCutoff {
            order: gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            seconds: since.timestamp(),
        })
        .all()
        .map_err(|e| err(path, e))?;
    let mut out = Vec::new();
    for info in walk {
        let Ok(info) = info else { break };
        let Ok(commit) = info.object() else { continue };
        let Ok(author) = commit.author() else { continue };
        let email = author.email.to_string().to_lowercase();
        if !emails.contains(&email) {
            continue;
        }
        let Some(at) = to_utc(author.seconds()) else {
            continue;
        };
        if at >= since {
            out.push((info.id.to_string(), at, email));
        }
    }
    Ok(out)
}

/// Repo-relative paths of changed tracked files (`M`/`A`/`D`/conflict, staged or not) and untracked files.
pub fn changed_paths(path: &Path) -> Result<(Vec<String>, Vec<String>), GitError> {
    let repo = gix::open(path).map_err(|e| err(path, e))?;
    let (mut tracked, mut untracked) = (Vec::new(), Vec::new());
    let status = repo.status(gix::progress::Discard).map_err(|e| err(path, e))?;
    for item in status.into_iter(None).map_err(|e| err(path, e))? {
        let Ok(item) = item else { continue };
        let (rela, is_untracked) = match &item {
            gix::status::Item::TreeIndex(c) => (c.location().to_string(), false),
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::iter::Summary;
                (
                    iw.rela_path().to_string(),
                    matches!(iw.summary(), Some(Summary::Added)),
                )
            }
        };
        if is_own_file(rela.as_bytes()) {
            continue;
        }
        let list = if is_untracked {
            &mut untracked
        } else {
            &mut tracked
        };
        if !list.contains(&rela) {
            list.push(rela);
        }
    }
    tracked.sort();
    untracked.sort();
    Ok((tracked, untracked))
}

/// Local branches whose tip is not reachable from the default branch (GIT-004).
pub fn unmerged_branches(path: &Path, default_branch: &str) -> Result<Vec<UnmergedBranch>, GitError> {
    let repo = gix::open(path).map_err(|e| err(path, e))?;
    let Ok(Some(mut base)) = repo.try_find_reference(format!("refs/heads/{default_branch}").as_str()) else {
        return Ok(Vec::new());
    };
    let base_id = base.peel_to_id().map_err(|e| err(path, e))?.detach();
    let mut out = Vec::new();
    let refs = repo.references().map_err(|e| err(path, e))?;
    for r in refs.local_branches().map_err(|e| err(path, e))? {
        let Ok(mut r) = r else { continue };
        let name = r.name().shorten().to_string();
        let Ok(tip) = r.peel_to_id().map(|id| id.detach()) else {
            continue;
        };
        if name == default_branch || tip == base_id {
            continue;
        }
        let merged = repo.merge_base(tip, base_id).ok().map(|b| b.detach()) == Some(tip);
        if !merged {
            let tip_at = repo
                .find_commit(tip)
                .ok()
                .and_then(|c| c.time().ok())
                .and_then(|t| to_utc(t.seconds));
            out.push(UnmergedBranch { name, tip_at });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Net `TODO`/`FIXME` markers added by the last `n` commits on HEAD (first-parent diffs; GIT-004).
/// Blobs over [`MAX_DIFF_BYTES`] (data files, bundles) are not read (PERF-001): their header size
/// is checked first, so they count as 0 without being decompressed.
pub fn todos_added(path: &Path, n: usize) -> Result<usize, GitError> {
    let mut repo = gix::open(path).map_err(|e| err(path, e))?;
    repo.object_cache_size_if_unset(4 * 1024 * 1024);
    let Ok(head) = repo.head_commit() else {
        return Ok(0);
    };
    let count = |id: gix::ObjectId| -> usize {
        if repo
            .find_header(id)
            .is_ok_and(|h| h.size() as usize > MAX_DIFF_BYTES)
        {
            return 0;
        }
        repo.find_object(id)
            .map(|o| {
                let text = String::from_utf8_lossy(&o.data);
                text.matches("TODO").count() + text.matches("FIXME").count()
            })
            .unwrap_or(0)
    };
    let mut total = 0;
    let walk = repo
        .rev_walk([head.id])
        .first_parent_only()
        .all()
        .map_err(|e| err(path, e))?;
    for info in walk.take(n) {
        let Ok(info) = info else { break };
        let Ok(commit) = info.object() else { continue };
        let Ok(tree) = commit.tree() else { continue };
        let parent_tree = commit
            .parent_ids()
            .next()
            .and_then(|p| p.object().ok()?.into_commit().tree().ok());
        let Ok(changes) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
            continue;
        };
        for change in changes {
            use gix::object::tree::diff::ChangeDetached as C;
            let (old, new) = match change {
                C::Addition { id, entry_mode, .. } if entry_mode.is_blob() => (0, count(id)),
                C::Modification {
                    previous_id,
                    id,
                    entry_mode,
                    ..
                } if entry_mode.is_blob() => (count(previous_id), count(id)),
                C::Rewrite {
                    source_id,
                    id,
                    entry_mode,
                    ..
                } if entry_mode.is_blob() => (count(source_id), count(id)),
                _ => (0, 0),
            };
            total += new.saturating_sub(old);
        }
    }
    Ok(total)
}

/// A local branch and how far it is ahead of its upstream (DEL-001: what a clone would not bring back).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchState {
    pub name: String,
    /// Short upstream name (`origin/main`), `None` for a local-only branch.
    pub upstream: Option<String>,
    /// Commits on the branch that the upstream doesn't have.
    pub ahead: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
    /// Browser link when the URL is a recognizable hosting URL (`git@host:o/r.git`, `https://…`).
    pub web_url: Option<String>,
}

/// Remotes (fetch URLs) and local branches with their upstream state.
pub fn remotes_and_branches(path: &Path) -> Result<(Vec<RemoteInfo>, Vec<BranchState>), GitError> {
    let repo = gix::open(path).map_err(|e| err(path, e))?;
    let mut remotes = Vec::new();
    for name in repo.remote_names() {
        let Ok(remote) = repo.find_remote(AsRef::<gix::bstr::BStr>::as_ref(&name)) else {
            continue;
        };
        if let Some(url) = remote.url(gix::remote::Direction::Fetch) {
            let url = url.to_bstring().to_string();
            remotes.push(RemoteInfo {
                name: name.to_string(),
                web_url: web_url(&url),
                url,
            });
        }
    }
    let mut branches = Vec::new();
    if let Ok(refs) = repo.references()
        && let Ok(locals) = refs.local_branches()
    {
        for mut r in locals.flatten() {
            let name = r.name().shorten().to_string();
            let Ok(tip) = r.peel_to_id() else { continue };
            let tip = tip.detach();
            let upstream = repo
                .branch_remote_tracking_ref_name(r.name(), gix::remote::Direction::Fetch)
                .and_then(Result::ok)
                .and_then(|u| {
                    let mut ur = repo.try_find_reference(u.as_ref()).ok().flatten()?;
                    Some((u.shorten().to_string(), ur.peel_to_id().ok()?.detach()))
                });
            let ahead = |hide: Option<gix::ObjectId>| {
                let mut walk = repo.rev_walk([tip]);
                if let Some(h) = hide {
                    walk = walk.with_hidden([h]);
                }
                walk.all().map(|w| w.count()).unwrap_or(0)
            };
            branches.push(match upstream {
                Some((up, id)) => BranchState {
                    name,
                    upstream: Some(up),
                    ahead: ahead(Some(id)),
                },
                None => BranchState {
                    name,
                    upstream: None,
                    ahead: ahead(None),
                },
            });
        }
    }
    branches.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((remotes, branches))
}

/// `https://host/owner/repo` for `git@host:owner/repo(.git)`, `ssh://git@host/owner/repo(.git)` and
/// `http(s)://[user@]host/owner/repo(.git)`; `None` for local paths and other schemes.
pub fn web_url(url: &str) -> Option<String> {
    let rest = if let Some(r) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        r.rsplit_once('@').map_or(r, |(_, h)| h).to_string()
    } else if let Some(r) = url.strip_prefix("ssh://") {
        let r = r.rsplit_once('@').map_or(r, |(_, h)| h);
        // Drop a port: host:22/owner/repo.
        match r.split_once('/') {
            Some((host, path)) => format!("{}/{path}", host.split(':').next().unwrap_or(host)),
            None => return None,
        }
    } else if let Some((user_host, path)) = url.split_once(':')
        && user_host.contains('@')
        && !path.starts_with('/')
    {
        format!(
            "{}/{path}",
            user_host.rsplit_once('@').map_or(user_host, |(_, h)| h)
        )
    } else {
        return None;
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    rest.contains('/').then(|| format!("https://{rest}"))
}

/// `origin/HEAD` target if set, else `main`, else `master` among local branches.
fn default_branch(repo: &gix::Repository) -> Option<String> {
    if let Ok(Some(r)) = repo.try_find_reference("refs/remotes/origin/HEAD")
        && let Some(target) = r.target().try_name()
    {
        let short = target.shorten().to_string();
        return Some(short.strip_prefix("origin/").unwrap_or(&short).to_string());
    }
    ["main", "master"]
        .into_iter()
        .find(|b| {
            repo.try_find_reference(format!("refs/heads/{b}").as_str())
                .ok()
                .flatten()
                .is_some()
        })
        .map(str::to_string)
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::Path;
    use std::process::Command;

    /// Run git with a fixed identity; panics on failure.
    pub fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git binary");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    pub fn write(dir: &Path, file: &str, content: &str) {
        std::fs::write(dir.join(file), content).unwrap();
    }

    /// Repo `work` with upstream `remote.git`, one pushed commit on `main`.
    pub fn repo_with_upstream(base: &Path) -> std::path::PathBuf {
        git(base, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
        git(base, &["init", "-q", "-b", "main", "work"]);
        let w = base.join("work");
        write(&w, "f.txt", "a\nb\nc\n");
        git(&w, &["add", "f.txt"]);
        git(&w, &["commit", "-qm", "c1"]);
        git(&w, &["remote", "add", "origin", "../remote.git"]);
        git(&w, &["push", "-q", "-u", "origin", "main"]);
        w
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn clean_repo_with_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        let s = scan_repo(&w).unwrap();
        assert!(s.last_commit_at.is_some());
        assert_eq!(
            (s.branch.as_deref(), s.default_branch.as_deref()),
            (Some("main"), Some("main"))
        );
        assert!(s.on_default_branch);
        assert_eq!(s.upstream.as_deref(), Some("refs/remotes/origin/main"));
        assert_eq!((s.ahead, s.behind, s.uncommitted_files()), (0, 0, 0));
    }

    #[test]
    fn dirty_ahead_and_feature_branch() {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        for i in 0..2 {
            write(&w, "f.txt", &format!("change {i}\n"));
            git(&w, &["commit", "-qam", "ahead"]);
        }
        git(&w, &["checkout", "-q", "-b", "feature"]);
        write(&w, "staged.txt", "s");
        git(&w, &["add", "staged.txt"]);
        write(&w, "f.txt", "modified\n");
        write(&w, "untracked.txt", "u");
        write(&w, "NEXT.md", "ignored own file");
        let s = scan_repo(&w).unwrap();
        assert_eq!(s.branch.as_deref(), Some("feature"));
        assert!(!s.on_default_branch);
        assert_eq!((s.staged, s.modified, s.untracked, s.conflicted), (1, 1, 1, 0));
        assert!(s.no_upstream(), "new local branch has no upstream");

        git(&w, &["checkout", "-q", "main"]);
        let s = scan_repo(&w).unwrap();
        assert_eq!((s.ahead, s.behind), (2, 0));
    }

    #[test]
    fn behind_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        write(&w, "f.txt", "x\n");
        git(&w, &["commit", "-qam", "c2"]);
        git(&w, &["push", "-q"]);
        git(&w, &["reset", "-q", "--hard", "HEAD~1"]);
        let s = scan_repo(&w).unwrap();
        assert_eq!((s.ahead, s.behind), (0, 1));
    }

    #[test]
    fn stash_and_in_progress_operations() {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        write(&w, "f.txt", "stash me\n");
        git(&w, &["stash", "-q"]);
        write(&w, "f.txt", "stash me too\n");
        git(&w, &["stash", "-q"]);
        let s = scan_repo(&w).unwrap();
        assert_eq!(s.stash_count, 2);
        assert!(s.oldest_stash_at.is_some());
        assert_eq!(s.operation, None);

        // Conflicting merge.
        git(&w, &["checkout", "-q", "-b", "other"]);
        write(&w, "c.txt", "other\n");
        git(&w, &["add", "c.txt"]);
        git(&w, &["commit", "-qm", "other"]);
        git(&w, &["checkout", "-q", "main"]);
        write(&w, "c.txt", "main\n");
        git(&w, &["add", "c.txt"]);
        git(&w, &["commit", "-qm", "main"]);
        let merge = std::process::Command::new("git")
            .args(["merge", "-q", "other"])
            .current_dir(&w)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(!merge.status.success());
        let s = scan_repo(&w).unwrap();
        assert_eq!((s.operation, s.conflicted), (Some(Operation::Merge), 1));
        git(&w, &["merge", "--abort"]);

        // Conflicting cherry-pick.
        let pick = std::process::Command::new("git")
            .args(["cherry-pick", "other"])
            .current_dir(&w)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(!pick.status.success());
        assert_eq!(scan_repo(&w).unwrap().operation, Some(Operation::CherryPick));
        git(&w, &["cherry-pick", "--abort"]);

        // Conflicting rebase.
        git(&w, &["checkout", "-q", "other"]);
        let rebase = std::process::Command::new("git")
            .args(["rebase", "main"])
            .current_dir(&w)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(!rebase.status.success());
        assert_eq!(scan_repo(&w).unwrap().operation, Some(Operation::Rebase));
    }

    #[test]
    fn unmerged_branches_and_todos() {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        git(&w, &["checkout", "-q", "-b", "merged"]);
        git(&w, &["checkout", "-q", "-b", "experiment"]);
        write(&w, "x.txt", "x");
        git(&w, &["add", "x.txt"]);
        git(&w, &["commit", "-qm", "exp"]);
        git(&w, &["checkout", "-q", "main"]);
        let names: Vec<String> = unmerged_branches(&w, "main")
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert_eq!(names, ["experiment"]);
        assert!(unmerged_branches(&w, "missing").unwrap().is_empty());

        write(&w, "a.rs", "// TODO one\n// FIXME two\n");
        git(&w, &["add", "a.rs"]);
        git(&w, &["commit", "-qm", "add todos"]);
        write(&w, "a.rs", "// TODO one\n");
        git(&w, &["commit", "-qam", "fix one"]);
        write(&w, "b.rs", "// TODO three\n");
        git(&w, &["add", "b.rs"]);
        git(&w, &["commit", "-qm", "another"]);
        assert_eq!(todos_added(&w, 1).unwrap(), 1);
        assert_eq!(
            todos_added(&w, 3).unwrap(),
            3,
            "removals do not offset other commits' additions"
        );
    }

    #[test]
    fn dirty_line_stats_and_oldest_change() {
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path()); // f.txt = a b c
        assert_eq!(dirty_stats(&w).unwrap(), DirtyStats::default());
        write(&w, "f.txt", "a\nB\nc\nd\n"); // unstaged: +2 -1
        write(&w, "new.txt", "1\n2\n3\n");
        git(&w, &["add", "new.txt"]); // staged: +3
        write(&w, "bin.dat", "\0\0binary");
        git(&w, &["add", "bin.dat"]); // binary: ignored for lines
        write(&w, "untracked.txt", "u\n"); // untracked: no lines, but has an mtime
        write(&w, "NEXT.md", "ignored\n");
        let old = SystemTime::now() - Duration::from_secs(20 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(w.join("untracked.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let s = dirty_stats(&w).unwrap();
        assert_eq!((s.lines_added, s.lines_removed), (5, 1));
        let oldest = s.oldest_change_at.unwrap();
        assert!(
            (Utc::now() - oldest).num_days() >= 19,
            "oldest dirty file is the untracked one"
        );
    }

    #[test]
    fn recent_commits_and_changed_paths() {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        write(&w, "f.txt", "x\n");
        git(&w, &["commit", "-qam", "second commit\n\nbody text"]);
        let commits = recent_commits(&w, 5).unwrap();
        let subjects: Vec<&str> = commits.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(subjects, ["second commit", "c1"]);
        write(&w, "f.txt", "y\n");
        write(&w, "s.txt", "s");
        git(&w, &["add", "s.txt"]);
        write(&w, "u.txt", "u");
        write(&w, "NEXT.md", "own");
        let (tracked, untracked) = changed_paths(&w).unwrap();
        assert_eq!(tracked, ["f.txt", "s.txt"]);
        assert_eq!(untracked, ["u.txt"]);
    }

    #[test]
    fn empty_repo_and_no_remote() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "trunk"]);
        let s = scan_repo(dir.path()).unwrap();
        assert_eq!(s.last_commit_at, None);
        assert_eq!(s.branch.as_deref(), Some("trunk"));
        assert!(s.no_upstream() && s.default_branch.is_none());
        assert!(scan_repo(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn web_urls_for_common_remote_forms() {
        assert_eq!(
            web_url("git@github.com:Zannael/logpose.git").as_deref(),
            Some("https://github.com/Zannael/logpose")
        );
        assert_eq!(
            web_url("https://github.com/o/r.git").as_deref(),
            Some("https://github.com/o/r")
        );
        assert_eq!(
            web_url("https://user:tok@gitlab.com/g/s/r").as_deref(),
            Some("https://gitlab.com/g/s/r")
        );
        assert_eq!(
            web_url("ssh://git@host.xz:2222/o/r.git/").as_deref(),
            Some("https://host.xz/o/r")
        );
        assert_eq!(web_url("../remote.git"), None);
        assert_eq!(web_url("/srv/git/r.git"), None);
        assert_eq!(web_url("file:///srv/r.git"), None);
    }

    #[test]
    fn remotes_and_branch_states() {
        let dir = tempfile::tempdir().unwrap();
        let w = repo_with_upstream(dir.path());
        write(&w, "g.txt", "x");
        git(&w, &["add", "g.txt"]);
        git(&w, &["commit", "-qm", "unpushed"]);
        git(&w, &["checkout", "-qb", "local-only"]);
        write(&w, "h.txt", "y");
        git(&w, &["add", "h.txt"]);
        git(&w, &["commit", "-qm", "local"]);
        let (remotes, branches) = remotes_and_branches(&w).unwrap();
        assert_eq!(remotes.len(), 1);
        assert_eq!(
            (
                remotes[0].name.as_str(),
                remotes[0].url.as_str(),
                remotes[0].web_url.as_deref()
            ),
            ("origin", "../remote.git", None)
        );
        assert_eq!(
            branches,
            [
                BranchState {
                    name: "local-only".into(),
                    upstream: None,
                    ahead: 3
                },
                BranchState {
                    name: "main".into(),
                    upstream: Some("origin/main".into()),
                    ahead: 1
                },
            ]
        );
    }
}
