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

/// A branch by name: a local branch and its remote-tracking copies
/// (`feat` and `origin/feat`) count as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// Without the remote: `feat`.
    pub name: String,
    pub local: bool,
    pub remote: bool,
    /// Every copy's commits are in the default branch.
    pub merged: bool,
    /// The newest copy's commit time, Unix seconds.
    pub time: i64,
}

/// Where a branch stands against the default branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fate {
    /// Its commits are in the default branch, or its GitHub PR was merged
    /// (which catches squash and rebase merges).
    Merged,
    /// Not merged, with an open or a draft PR.
    InReview,
    /// Not merged and no open PR: work that didn't make it, or hasn't yet.
    Hanging,
}

/// The branches other than the default one, sorted into their fates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchSummary {
    pub merged: Vec<Branch>,
    pub in_review: Vec<Branch>,
    /// Oldest first: the likeliest to be forgotten.
    pub hanging: Vec<Branch>,
}

impl BranchSummary {
    pub fn total(&self) -> usize {
        self.merged.len() + self.in_review.len() + self.hanging.len()
    }
}

/// Sorts `branches` by fate, with `pr` giving each branch name's PR state.
pub fn summarize(
    branches: &[Branch],
    pr: impl Fn(&str) -> Option<crate::graph::PrState>,
) -> BranchSummary {
    use crate::graph::PrState;
    let mut s = BranchSummary::default();
    for b in branches {
        let fate = match pr(&b.name) {
            _ if b.merged => Fate::Merged,
            Some(PrState::Merged) => Fate::Merged,
            Some(PrState::Open | PrState::Draft) => Fate::InReview,
            Some(PrState::Closed) | None => Fate::Hanging,
        };
        match fate {
            Fate::Merged => s.merged.push(b.clone()),
            Fate::InReview => s.in_review.push(b.clone()),
            Fate::Hanging => s.hanging.push(b.clone()),
        }
    }
    s.hanging.sort_by_key(|b| b.time);
    s.in_review.sort_by_key(|b| std::cmp::Reverse(b.time));
    s.merged.sort_by_key(|b| std::cmp::Reverse(b.time));
    s
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
    /// Every branch but the default one, local and remote merged by name.
    pub branches: Vec<Branch>,
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

/// What `read` depends on that can change while no ref moves: the
/// worktrees (added, removed, or a detached one committed to) and where
/// `origin/HEAD` points. Cheap to read, so a check can tell whether `Info`
/// is still good.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    worktrees: Option<String>,
    origin_head: Option<String>,
}

pub fn setup(repo: &Path) -> Setup {
    Setup {
        worktrees: text(repo, &["worktree", "list", "--porcelain"]),
        origin_head: text(repo, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]),
    }
}

pub fn read(repo: &Path, refs: &Refs, setup: &Setup) -> Result<Info, String> {
    let checkout = match (&refs.head_branch, &refs.head) {
        (Some(branch), Some(_)) => Checkout::Branch(branch.clone()),
        (Some(branch), None) => Checkout::Unborn(branch.clone()),
        (None, Some(id)) => Checkout::Detached(id[..id.len().min(7)].to_string()),
        (None, None) => Checkout::Detached(String::new()),
    };

    let worktrees = setup
        .worktrees
        .as_deref()
        .map(parse_worktrees)
        .unwrap_or_default();

    let all: Vec<&Ref> = refs.all().collect();
    let local_branches = all.iter().filter(|r| matches!(r, Ref::Local(_))).count();
    let remote_branches = all.iter().filter(|r| matches!(r, Ref::Remote(_))).count();

    let default_branch = default_branch(setup, refs);
    let default_tip = default_branch.as_ref().and_then(|name| {
        let r = if refs.tip(&Ref::Remote(name.clone())).is_some() {
            Ref::Remote(name.clone())
        } else {
            Ref::Local(name.clone())
        };
        refs.tip(&r).map(str::to_string)
    });
    let last_merge = default_tip.as_deref().and_then(|tip| last_merge(repo, tip));
    let branches = branches(repo, default_tip.as_deref(), default_branch.as_deref());

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
        branches,
        unpushed,
        unpulled,
    })
}

/// The repo's branches by name, with which are merged into `tip` (the
/// default branch's commit). Two read-only `for-each-ref` calls.
fn branches(repo: &Path, tip: Option<&str>, default: Option<&str>) -> Vec<Branch> {
    const FORMAT: &str = "--format=%(refname)%1f%(committerdate:unix)";
    let Some(all) = text(
        repo,
        &["for-each-ref", FORMAT, "refs/heads", "refs/remotes"],
    ) else {
        return Vec::new();
    };
    let merged = tip
        .filter(|t| t.bytes().all(|b| b.is_ascii_hexdigit()))
        .and_then(|t| {
            let arg = format!("--merged={t}");
            text(
                repo,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    &arg,
                    "refs/heads",
                    "refs/remotes",
                ],
            )
        })
        .unwrap_or_default();
    parse_branches(&all, &merged, default)
}

/// `all` is `refname\x1ftime` lines, `merged` the merged refnames. The
/// default branch (`origin/main` or `main`) is left out by its short name.
pub(crate) fn parse_branches(all: &str, merged: &str, default: Option<&str>) -> Vec<Branch> {
    let merged: HashSet<&str> = merged.lines().map(str::trim).collect();
    let default = default.map(|d| d.split_once('/').map_or(d, |(_, n)| n));
    let mut by_name: std::collections::BTreeMap<String, Branch> = Default::default();
    for line in all.lines() {
        let Some((refname, time)) = line.split_once('\x1f') else {
            continue;
        };
        let (name, local) = if let Some(n) = refname.strip_prefix("refs/heads/") {
            (n, true)
        } else if let Some(n) = refname.strip_prefix("refs/remotes/") {
            match n.split_once('/') {
                Some((_, "HEAD")) | None => continue,
                Some((_, n)) => (n, false),
            }
        } else {
            continue;
        };
        if Some(name) == default {
            continue;
        }
        let time = time.trim().parse().unwrap_or(0);
        let is_merged = merged.contains(refname);
        let b = by_name.entry(name.to_string()).or_insert(Branch {
            name: name.to_string(),
            local: false,
            remote: false,
            merged: true,
            time,
        });
        b.local |= local;
        b.remote |= !local;
        b.merged &= is_merged;
        b.time = b.time.max(time);
    }
    by_name.into_values().collect()
}

/// What sorting the repos needs from one, read on every check.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SortFacts {
    pub last_commit: i64,
    pub last_worked: i64,
    pub unmerged: Vec<String>,
}

/// Three read-only git calls: every branch with its commit time, the ones
/// merged into the default branch, and HEAD's newest reflog entry.
pub fn sort_facts(repo: &Path) -> SortFacts {
    const FORMAT: &str = "--format=%(refname)%1f%(committerdate:unix)";
    let all = text(
        repo,
        &["for-each-ref", FORMAT, "refs/heads", "refs/remotes"],
    )
    .unwrap_or_default();
    let origin_head = text(repo, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]);
    let refnames: Vec<&str> = all.lines().filter_map(|l| l.split('\x1f').next()).collect();
    let default = default_ref(origin_head.as_deref(), &refnames);
    let merged = default
        .as_deref()
        .and_then(|d| {
            let arg = format!("--merged={d}");
            text(
                repo,
                &[
                    "for-each-ref",
                    "--format=%(refname)",
                    &arg,
                    "refs/heads",
                    "refs/remotes",
                ],
            )
        })
        .unwrap_or_default();
    let short = default.as_deref().map(|d| {
        d.strip_prefix("refs/remotes/")
            .or_else(|| d.strip_prefix("refs/heads/"))
            .unwrap_or(d)
            .to_string()
    });
    let branches = parse_branches(&all, &merged, short.as_deref());
    let last_worked = text(
        repo,
        &[
            "log",
            "-g",
            "-1",
            "--date=unix",
            "--format=%gd",
            "HEAD",
            "--",
        ],
    )
    .and_then(|t| reflog_time(&t))
    .unwrap_or(0);
    SortFacts {
        last_commit: all
            .lines()
            .filter_map(|l| l.split('\x1f').nth(1)?.trim().parse().ok())
            .max()
            .unwrap_or(0),
        last_worked,
        unmerged: branches
            .into_iter()
            .filter(|b| !b.merged)
            .map(|b| b.name)
            .collect(),
    }
}

/// The default branch's full refname: what `origin/HEAD` points at, else
/// the first of the usual names among `refnames`.
fn default_ref(origin_head: Option<&str>, refnames: &[&str]) -> Option<String> {
    if let Some(r) = origin_head
        .map(str::trim)
        .filter(|r| r.starts_with("refs/"))
    {
        return Some(r.to_string());
    }
    [
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
        "refs/heads/main",
        "refs/heads/master",
    ]
    .into_iter()
    .find(|c| refnames.contains(c))
    .map(str::to_string)
}

/// The time in `HEAD@{1730000000}`, as `log -g --date=unix --format=%gd` prints.
fn reflog_time(text: &str) -> Option<i64> {
    let t = text.trim();
    t.strip_prefix("HEAD@{")?.strip_suffix('}')?.parse().ok()
}

fn text(repo: &Path, args: &[&str]) -> Option<String> {
    let out = git::git(repo, args, git::LOCAL_TIMEOUT).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `origin/HEAD`'s branch, else the first of the usual names that exists.
fn default_branch(setup: &Setup, refs: &Refs) -> Option<String> {
    if let Some(name) = setup
        .origin_head
        .as_deref()
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

/// The GitHub page of `repo`'s `origin`, read without changing anything.
pub fn origin_web(repo: &Path) -> Option<String> {
    read_origin_web(repo).ok().flatten()
}

/// Like [`origin_web`], but an error when git couldn't be asked (it timed
/// out or didn't start), so "no GitHub origin" is a real answer.
pub fn read_origin_web(repo: &Path) -> Result<Option<String>, String> {
    let out = git::git(repo, &["remote", "get-url", "origin"], git::LOCAL_TIMEOUT)?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(github_web(String::from_utf8_lossy(&out.stdout).trim()))
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
        run(&remote, &["checkout", "-q", "-b", "spike"]);
        commit(&remote, "spike", &mut n);
        run(&remote, &["checkout", "-q", "main"]);

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
        let info = read(&dir, &refs, &setup(&dir)).unwrap();
        assert_eq!(info.checkout, Checkout::Branch("main".into()));
        assert_eq!(info.worktrees.len(), 2);
        assert_eq!(info.local_branches, 1);
        // origin/main, origin/feature and origin/spike; origin/HEAD isn't counted.
        assert_eq!(info.remote_branches, 3);
        let fates: Vec<(&str, bool, bool)> = info
            .branches
            .iter()
            .map(|b| (b.name.as_str(), b.remote && !b.local, b.merged))
            .collect();
        assert_eq!(fates, [("feature", true, true), ("spike", true, false)]);
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
    fn parses_branches_merging_copies_by_name() {
        let all = "refs/heads/feat\x1f100\n\
                   refs/remotes/origin/feat\x1f200\n\
                   refs/remotes/origin/HEAD\x1f300\n\
                   refs/remotes/origin/main\x1f300\n\
                   refs/heads/main\x1f290\n\
                   refs/heads/done\x1f50\n\
                   refs/remotes/upstream/team/x\x1f70\n\
                   garbage\n";
        // Only the local feat is merged; origin/feat has more, so the
        // branch isn't.
        let merged = "refs/heads/feat\nrefs/heads/done\n";
        let b = parse_branches(all, merged, Some("origin/main"));
        let names: Vec<&str> = b.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["done", "feat", "team/x"]);
        assert!(b[0].merged && b[0].local && !b[0].remote);
        assert!(!b[1].merged && b[1].local && b[1].remote);
        assert_eq!(b[1].time, 200);
        assert!(!b[2].merged && b[2].remote);
        // A repo with no remote names its default branch plainly.
        let local = parse_branches(
            "refs/heads/main\x1f1\nrefs/heads/x\x1f2\n",
            "",
            Some("main"),
        );
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].name, "x");
    }

    #[test]
    fn sort_facts_of_a_repo() {
        let dir = temp_repo("sort-facts");
        let mut n = 0;
        commit(&dir, "one", &mut n);
        run(&dir, &["checkout", "-q", "-b", "wip"]);
        commit(&dir, "two", &mut n);
        run(&dir, &["checkout", "-q", "main"]);
        let f = sort_facts(&dir);
        // `commit` dates commits 60 s apart from 1_700_000_000.
        assert_eq!(f.last_commit, 1_700_000_120);
        assert_eq!(f.unmerged, ["wip"]);
        assert!(f.last_worked > 1_700_000_000, "{}", f.last_worked);
        assert_eq!(reflog_time("HEAD@{1730000000}\n"), Some(1_730_000_000));
        assert_eq!(reflog_time(""), None);
        assert_eq!(
            default_ref(None, &["refs/heads/main", "refs/heads/x"]).as_deref(),
            Some("refs/heads/main")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summarize_sorts_branches_by_fate() {
        use crate::graph::PrState;
        let b = |name: &str, merged: bool, time: i64| Branch {
            name: name.into(),
            local: true,
            remote: false,
            merged,
            time,
        };
        let branches = [
            b("merged", true, 5),
            b("squashed", false, 4),
            b("review", false, 3),
            b("draft", false, 2),
            b("closed", false, 1),
            b("old", false, 0),
            b("new", false, 9),
        ];
        let s = summarize(&branches, |name| match name {
            "squashed" => Some(PrState::Merged),
            "review" | "merged" => Some(PrState::Open),
            "draft" => Some(PrState::Draft),
            "closed" => Some(PrState::Closed),
            _ => None,
        });
        let names = |v: &[Branch]| v.iter().map(|b| b.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&s.merged), ["merged", "squashed"]);
        assert_eq!(names(&s.in_review), ["review", "draft"]);
        assert_eq!(names(&s.hanging), ["old", "closed", "new"]);
        assert_eq!(s.total(), 7);
    }

    #[test]
    fn repo_without_remote_or_commits() {
        let dir = temp_repo("info-bare");
        let refs = history::refs(&dir).unwrap();
        let info = read(&dir, &refs, &setup(&dir)).unwrap();
        assert_eq!(info.checkout, Checkout::Unborn("main".into()));
        assert_eq!(info.default_branch, None);
        assert!(info.unpushed.is_empty());

        let mut n = 0;
        commit(&dir, "one", &mut n);
        let refs = history::refs(&dir).unwrap();
        let info = read(&dir, &refs, &setup(&dir)).unwrap();
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert_eq!(info.default_tip, refs.head);
        // No remote: nothing is "unpushed".
        assert!(info.unpushed.is_empty());
        assert_eq!(info.last_merge, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
