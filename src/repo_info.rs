//! What a repo's column header shows: the checkout, worktrees, branch
//! counts and the last merge, plus the commits that are unpushed or
//! unpulled, which the graph colours.

use crate::git;
use crate::history::{Ref, Refs};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The most unpushed or unpulled commits marked. Past this the colour stops,
/// but the header's ahead/behind counts stay exact.
const MAX_MARKED: usize = 5000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checkout {
    Branch(String),
    /// A detached HEAD, by short hash.
    Detached(String),
    /// A branch with no commits yet.
    Unborn(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Option<String>,
    /// `None` when detached.
    pub branch: Option<String>,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merge {
    pub id: String,
    pub time: i64,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub checkout: Checkout,
    pub worktrees: Vec<Worktree>,
    pub local_branches: usize,
    pub remote_branches: usize,
    /// `origin/main`, or a local `main` in a repo with no remote.
    pub default_branch: Option<String>,
    pub default_tip: Option<String>,
    pub last_merge: Option<Merge>,
    pub unpushed: HashSet<String>,
    pub unpulled: HashSet<String>,
    /// `https://github.com/owner/repo` for a GitHub `origin`.
    pub web: Option<String>,
}

impl Info {
    /// The HEADs of worktrees other than the current one: history to walk
    /// that `--all` may not reach (a detached worktree).
    pub fn worktree_tips(&self, head: Option<&str>) -> Vec<String> {
        self.worktrees
            .iter()
            .filter_map(|w| w.head.clone())
            .filter(|h| Some(h.as_str()) != head)
            .collect()
    }
}

pub fn read(repo: &Path, refs: &Refs) -> Result<Info, String> {
    let checkout = match (&refs.head_branch, &refs.head) {
        (Some(branch), Some(_)) => Checkout::Branch(branch.clone()),
        (Some(branch), None) => Checkout::Unborn(branch.clone()),
        (None, Some(id)) => Checkout::Detached(id[..id.len().min(7)].to_string()),
        (None, None) => Checkout::Detached(String::new()),
    };

    let worktrees = text(repo, &["worktree", "list", "--porcelain"])
        .map(|t| parse_worktrees(&t))
        .unwrap_or_default();

    let all: Vec<&Ref> = refs.all().collect();
    let local_branches = all.iter().filter(|r| matches!(r, Ref::Local(_))).count();
    let remote_branches = all.iter().filter(|r| matches!(r, Ref::Remote(_))).count();

    let default_branch = default_branch(repo, refs);
    let default_tip = default_branch.as_ref().and_then(|name| {
        let r = if refs.tip(&Ref::Remote(name.clone())).is_some() {
            Ref::Remote(name.clone())
        } else {
            Ref::Local(name.clone())
        };
        refs.tip(&r).map(str::to_string)
    });
    let last_merge = default_tip.as_deref().and_then(|tip| last_merge(repo, tip));

    let (unpushed, unpulled) = if refs.head.is_some() {
        sync_sets(repo, remote_branches > 0)
    } else {
        Default::default()
    };

    let web = text(repo, &["remote", "get-url", "origin"]).and_then(|u| github_web(u.trim()));

    Ok(Info {
        web,
        checkout,
        worktrees,
        local_branches,
        remote_branches,
        default_branch,
        default_tip,
        last_merge,
        unpushed,
        unpulled,
    })
}

fn text(repo: &Path, args: &[&str]) -> Option<String> {
    let out = git::git(repo, args, git::LOCAL_TIMEOUT).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `origin/HEAD`'s branch, else the first of the usual names that exists.
fn default_branch(repo: &Path, refs: &Refs) -> Option<String> {
    if let Some(name) = text(repo, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"])
        .and_then(|t| t.trim().strip_prefix("refs/remotes/").map(str::to_string))
    {
        return Some(name);
    }
    let candidates = [
        Ref::Remote("origin/main".into()),
        Ref::Remote("origin/master".into()),
        Ref::Local("main".into()),
        Ref::Local("master".into()),
    ];
    candidates
        .into_iter()
        .find(|r| refs.tip(r).is_some())
        .map(|r| r.name().to_string())
}

fn last_merge(repo: &Path, tip: &str) -> Option<Merge> {
    let out = text(
        repo,
        &[
            "log",
            "-1",
            "--merges",
            "--first-parent",
            "--format=%H%x1f%at%x1f%s",
            tip,
            "--",
        ],
    )?;
    let mut f = out.trim_end_matches('\n').splitn(3, '\x1f');
    let id = f.next()?.to_string();
    if id.is_empty() {
        return None;
    }
    Some(Merge {
        id,
        time: f.next()?.parse().unwrap_or(0),
        subject: f.next().unwrap_or_default().to_string(),
    })
}

/// Commits on HEAD but not its upstream, and the other way round. A branch
/// with no upstream counts what no remote branch has; in a repo with no
/// remotes at all nothing counts as unpushed.
fn sync_sets(repo: &Path, has_remotes: bool) -> (HashSet<String>, HashSet<String>) {
    let max = format!("--max-count={MAX_MARKED}");
    if let Some(out) = text(
        repo,
        &["rev-list", "--left-right", &max, "HEAD...@{upstream}", "--"],
    ) {
        return parse_left_right(&out);
    }
    if !has_remotes {
        return Default::default();
    }
    let unpushed = text(
        repo,
        &["rev-list", &max, "HEAD", "--not", "--remotes", "--"],
    )
    .map(|t| t.lines().map(str::to_string).collect())
    .unwrap_or_default();
    (unpushed, HashSet::new())
}

/// The GitHub page of a remote URL, from its https, ssh or scp form.
pub fn github_web(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    (parts.next().is_none() && ok(owner) && ok(repo))
        .then(|| format!("https://github.com/{owner}/{repo}"))
}

fn parse_left_right(out: &str) -> (HashSet<String>, HashSet<String>) {
    let mut left = HashSet::new();
    let mut right = HashSet::new();
    for line in out.lines() {
        if let Some(id) = line.strip_prefix('<') {
            left.insert(id.to_string());
        } else if let Some(id) = line.strip_prefix('>') {
            right.insert(id.to_string());
        }
    }
    (left, right)
}

fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut list = Vec::new();
    for block in out.split("\n\n") {
        let mut w = Worktree::default();
        let mut seen = false;
        for line in block.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "worktree" => {
                    w.path = PathBuf::from(value);
                    seen = true;
                }
                // An unborn branch's HEAD is all zeros: no commit to walk.
                "HEAD" if value.bytes().any(|b| b != b'0') => w.head = Some(value.to_string()),
                "branch" => {
                    w.branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_string(),
                    )
                }
                "bare" => w.bare = true,
                "locked" => w.locked = true,
                "prunable" => w.prunable = true,
                _ => {}
            }
        }
        if seen {
            list.push(w);
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::tests::{commit, run, temp_repo};
    use crate::history::{self};

    #[test]
    fn parses_worktree_list() {
        let out = "worktree /r\nHEAD aaa\nbranch refs/heads/main\n\n\
                   worktree /r-wt\nHEAD bbb\ndetached\nlocked reason here\n\n\
                   worktree /gone\nHEAD ccc\nbranch refs/heads/x\nprunable gitdir file points to non-existent location\n";
        let w = parse_worktrees(out);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert_eq!(w[1].branch, None);
        assert!(w[1].locked && !w[1].prunable);
        assert!(w[2].prunable);
        assert_eq!(w[2].path, PathBuf::from("/gone"));
        assert!(parse_worktrees("").is_empty());

        // An unborn branch has no commit to walk.
        let unborn = format!(
            "worktree /u\nHEAD {}\nbranch refs/heads/main\n",
            "0".repeat(40)
        );
        assert_eq!(parse_worktrees(&unborn)[0].head, None);
    }

    #[test]
    fn github_web_urls() {
        let want = Some("https://github.com/oleroma/gitstat".to_string());
        assert_eq!(github_web("https://github.com/oleroma/gitstat.git"), want);
        assert_eq!(github_web("https://github.com/oleroma/gitstat/"), want);
        assert_eq!(github_web("git@github.com:oleroma/gitstat.git"), want);
        assert_eq!(github_web("ssh://git@github.com/oleroma/gitstat"), want);
        assert_eq!(github_web("https://gitlab.com/a/b"), None);
        assert_eq!(github_web("https://github.com/a/b/c"), None);
        assert_eq!(github_web("https://github.com/a/b?x=<y>"), None);
    }

    #[test]
    fn parses_left_right() {
        let (l, r) = parse_left_right("<a\n<b\n>c\n");
        assert_eq!(l.len(), 2);
        assert!(r.contains("c"));
    }

    #[test]
    fn reads_a_repo_with_worktrees_merges_and_a_remote() {
        let remote = temp_repo("info-remote");
        let mut n = 0;
        commit(&remote, "root", &mut n);
        run(&remote, &["checkout", "-q", "-b", "feature"]);
        commit(&remote, "feat", &mut n);
        run(&remote, &["checkout", "-q", "main"]);
        commit(&remote, "trunk", &mut n);
        run(
            &remote,
            &["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"],
        );

        let dir = std::env::temp_dir().join(format!("gitstat-info-clone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        run(
            &std::env::temp_dir(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                dir.to_str().unwrap(),
            ],
        );
        commit(&dir, "local only", &mut n);
        let wt = std::env::temp_dir().join(format!("gitstat-info-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        run(
            &dir,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                wt.to_str().unwrap(),
                "HEAD~1",
            ],
        );

        let refs = history::refs(&dir).unwrap();
        let info = read(&dir, &refs).unwrap();
        assert_eq!(info.checkout, Checkout::Branch("main".into()));
        assert_eq!(info.worktrees.len(), 2);
        assert_eq!(info.local_branches, 1);
        // origin/main and origin/feature; origin/HEAD isn't counted.
        assert_eq!(info.remote_branches, 2);
        assert_eq!(info.default_branch.as_deref(), Some("origin/main"));
        let merge = info.last_merge.as_ref().unwrap();
        assert_eq!(merge.subject, "Merge feature");
        assert_eq!(info.unpushed.len(), 1);
        assert!(info.unpushed.contains(refs.head.as_deref().unwrap()));
        assert!(info.unpulled.is_empty());
        let tips = info.worktree_tips(refs.head.as_deref());
        assert_eq!(tips.len(), 1);

        for d in [&remote, &dir, &wt] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn repo_without_remote_or_commits() {
        let dir = temp_repo("info-bare");
        let refs = history::refs(&dir).unwrap();
        let info = read(&dir, &refs).unwrap();
        assert_eq!(info.checkout, Checkout::Unborn("main".into()));
        assert_eq!(info.default_branch, None);
        assert!(info.unpushed.is_empty());

        let mut n = 0;
        commit(&dir, "one", &mut n);
        let refs = history::refs(&dir).unwrap();
        let info = read(&dir, &refs).unwrap();
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert_eq!(info.default_tip, refs.head);
        // No remote: nothing is "unpushed".
        assert!(info.unpushed.is_empty());
        assert_eq!(info.last_merge, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
