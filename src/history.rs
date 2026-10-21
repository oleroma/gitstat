//! A repo's commit history, read page by page for the timeline graph.
//!
//! A page is `git log --all --date-order` from where the last one ended, so
//! every commit comes after all its children: the order `graph::Layout`
//! needs. Pages are checked by hash rather than trusted by position: the
//! next page re-reads the last commit already loaded, and if it isn't the
//! first one back (history moved underneath us), the caller starts over.
//!
//! Refs are read apart from the walk and looked up by hash when drawing: a
//! loaded commit never changes, but a branch moves on every commit.
//!
//! Stashes are left out of the walk (a stash is two or three commits of
//! index and untracked files that would clutter the graph) and read
//! separately, to hang off the commit each one was made on.

// Wired into the tray and window views in a later milestone.
#![allow(dead_code)]

use crate::git;
use std::collections::HashMap;
use std::path::Path;

/// Commits per page.
pub const PAGE_SIZE: usize = 300;

/// Field and record separators in `git log` output. Neither can appear in a
/// ref name, a hash or a date, and real subjects don't use them.
const FS: char = '\x1f';
const RS: char = '\x1e';
const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%s%x1e";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    /// First parent first; empty for a root commit.
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    /// Author time, Unix seconds.
    pub time: i64,
    pub subject: String,
}

impl Commit {
    pub fn short(&self) -> &str {
        &self.id[..self.id.len().min(7)]
    }

    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ref {
    Local(String),
    /// `origin/main`, without `refs/remotes/`.
    Remote(String),
    Tag(String),
}

impl Ref {
    pub fn name(&self) -> &str {
        match self {
            Ref::Local(n) | Ref::Remote(n) | Ref::Tag(n) => n,
        }
    }
}

/// Every branch and tag, by the commit it points at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refs {
    pub by_commit: HashMap<String, Vec<Ref>>,
    /// The commit HEAD points at; `None` in a repo with no commits.
    pub head: Option<String>,
    /// The branch HEAD is on; `None` when detached.
    pub head_branch: Option<String>,
}

impl Refs {
    pub fn at(&self, id: &str) -> &[Ref] {
        self.by_commit.get(id).map_or(&[], Vec::as_slice)
    }

    pub fn is_head(&self, id: &str) -> bool {
        self.head.as_deref() == Some(id)
    }

    /// The commit a branch or tag points at.
    pub fn tip(&self, r: &Ref) -> Option<&str> {
        self.by_commit
            .iter()
            .find(|(_, refs)| refs.contains(r))
            .map(|(id, _)| id.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    pub id: String,
    /// The commit the stash was made on.
    pub base: String,
    /// `stash@{0}`.
    pub name: String,
    pub time: i64,
    pub subject: String,
}

/// One page of history, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub commits: Vec<Commit>,
    /// Fewer than a full page came back: this is the end of history.
    pub end: bool,
}

/// Why the next page couldn't be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageError {
    /// The history changed since the earlier pages: reload from the top.
    Moved,
    Git(String),
}

/// The first page. `tips` are extra starting points `--all` misses, such
/// as the HEADs of other worktrees.
pub fn first_page(repo: &Path, tips: &[String], n: usize) -> Result<Page, String> {
    let out = log(repo, tips, 0, n)?;
    let commits = parse_log(&out);
    Ok(Page {
        end: commits.len() < n,
        commits,
    })
}

/// The page after `loaded` commits, the last of which was `last`.
pub fn next_page(
    repo: &Path,
    tips: &[String],
    loaded: usize,
    last: &str,
    n: usize,
) -> Result<Page, PageError> {
    let skip = loaded.saturating_sub(1);
    let out = log(repo, tips, skip, n + 1).map_err(PageError::Git)?;
    let mut commits = parse_log(&out);
    if commits.first().map(|c| c.id.as_str()) != Some(last) {
        return Err(PageError::Moved);
    }
    commits.remove(0);
    Ok(Page {
        end: commits.len() < n,
        commits,
    })
}

/// Splices a fresh first page onto history already loaded. Returns the
/// commits that are new, newest first, or `None` when the fresh page no
/// longer reaches the newest loaded commit (a rebase, a force push, a
/// deleted branch) and everything has to be reloaded.
pub fn new_since(fresh: &Page, loaded: &[Commit]) -> Option<Vec<Commit>> {
    let Some(newest) = loaded.first() else {
        return Some(fresh.commits.clone());
    };
    let at = fresh.commits.iter().position(|c| c.id == newest.id)?;
    // The overlap must be in the same order too: a commit that is new but
    // older than the newest loaded one (a branch fetched with old commits)
    // would land in the middle of loaded history.
    let overlap = fresh.commits[at..].iter().zip(loaded);
    if overlap.into_iter().any(|(a, b)| a.id != b.id) {
        return None;
    }
    Some(fresh.commits[..at].to_vec())
}

pub fn refs(repo: &Path) -> Result<Refs, String> {
    let out = git::git(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname)%1f%(*objectname)%1f%(refname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
        git::LOCAL_TIMEOUT,
    )?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let mut refs = parse_refs(&String::from_utf8_lossy(&out.stdout));
    let rev = |args: &[&str]| -> Option<String> {
        let out = git::git(repo, args, git::LOCAL_TIMEOUT).ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !text.is_empty()).then_some(text)
    };
    refs.head = rev(&["rev-parse", "-q", "--verify", "HEAD^{commit}"]);
    refs.head_branch = rev(&["symbolic-ref", "-q", "--short", "HEAD"]);
    Ok(refs)
}

pub fn stashes(repo: &Path) -> Result<Vec<Stash>, String> {
    let out = git::git(
        repo,
        &[
            "log",
            "-g",
            "--no-color",
            "--format=%H%x1f%P%x1f%gd%x1f%at%x1f%s%x1e",
            "refs/stash",
            "--",
        ],
        git::LOCAL_TIMEOUT,
    )?;
    // No stash ref: nothing stashed.
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(parse_stashes(&String::from_utf8_lossy(&out.stdout)))
}

fn log(repo: &Path, tips: &[String], skip: usize, n: usize) -> Result<String, String> {
    let skip = format!("--skip={skip}");
    let max = format!("--max-count={n}");
    let mut args = vec![
        "log",
        "--no-color",
        "--date-order",
        LOG_FORMAT,
        &skip,
        &max,
        // Before --all, which it applies to.
        "--exclude=refs/stash",
        "--all",
    ];
    // Only full hashes, so a tip can't be read as an option.
    args.extend(
        tips.iter()
            .filter(|t| t.len() >= 40 && t.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(String::as_str),
    );
    args.push("--");
    let out = git::git(repo, &args, git::LOCAL_TIMEOUT)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        // A repo with no commits yet has an empty history, not an error.
        if err.contains("does not have any commits") || err.contains("bad default revision") {
            return Ok(String::new());
        }
        return Err(err.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn parse_log(out: &str) -> Vec<Commit> {
    out.split(RS)
        .filter_map(|record| {
            let record = record.trim_start_matches('\n');
            let mut f = record.split(FS);
            let id = f.next()?.to_string();
            if id.is_empty() {
                return None;
            }
            let parents = f.next()?.split_whitespace().map(str::to_string).collect();
            let author = f.next()?.to_string();
            let email = f.next()?.to_string();
            let time = f.next()?.parse().unwrap_or(0);
            // The rest, in case a subject holds the separator.
            let subject = f.collect::<Vec<_>>().join("\u{1f}");
            Some(Commit {
                id,
                parents,
                author,
                email,
                time,
                subject,
            })
        })
        .collect()
}

/// `for-each-ref` lines: the ref's object, the commit an annotated tag
/// points at (empty otherwise), and the full ref name.
fn parse_refs(out: &str) -> Refs {
    let mut refs = Refs::default();
    for line in out.lines() {
        let mut f = line.split(FS);
        let (Some(object), Some(peeled), Some(name)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let target = if peeled.is_empty() { object } else { peeled };
        let r = if let Some(n) = name.strip_prefix("refs/heads/") {
            Ref::Local(n.to_string())
        } else if let Some(n) = name.strip_prefix("refs/remotes/") {
            // `origin/HEAD` only repeats the default branch.
            if n.ends_with("/HEAD") {
                continue;
            }
            Ref::Remote(n.to_string())
        } else if let Some(n) = name.strip_prefix("refs/tags/") {
            Ref::Tag(n.to_string())
        } else {
            continue;
        };
        refs.by_commit
            .entry(target.to_string())
            .or_default()
            .push(r);
    }
    refs
}

fn parse_stashes(out: &str) -> Vec<Stash> {
    out.split(RS)
        .filter_map(|record| {
            let record = record.trim_start_matches('\n');
            let mut f = record.split(FS);
            let id = f.next()?.to_string();
            let base = f.next()?.split_whitespace().next()?.to_string();
            let name = f.next()?.to_string();
            let time = f.next()?.parse().unwrap_or(0);
            let subject = f.next().unwrap_or_default().to_string();
            (!id.is_empty()).then_some(Stash {
                id,
                base,
                name,
                time,
                subject,
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    pub fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Commits with strictly increasing dates, so `--date-order` is fixed.
    pub fn commit(dir: &Path, msg: &str, n: &mut i64) {
        *n += 1;
        let date = format!("@{} +0000", 1_700_000_000 + *n * 60);
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "--allow-empty", "-m", msg])
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .status()
            .unwrap();
        assert!(out.success());
    }

    pub fn temp_repo(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitstat-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]);
        dir
    }

    #[test]
    fn parses_log_records() {
        let out = "aaa\x1fbbb ccc\x1fAnn\x1fa@x\x1f1700000000\x1fMerge it\x1e\n\
                   bbb\x1f\x1fBob\x1fb@x\x1f1699999999\x1fsubject with \x1f odd\x1e\n";
        let commits = parse_log(out);
        assert_eq!(commits.len(), 2);
        let c = &commits[0];
        assert_eq!(c.parents, vec!["bbb", "ccc"]);
        assert!(c.is_merge());
        assert_eq!(c.time, 1_700_000_000);
        assert_eq!(c.subject, "Merge it");
        assert!(commits[1].parents.is_empty());
        assert_eq!(commits[1].subject, "subject with \x1f odd");
        assert_eq!(parse_log(""), Vec::new());
    }

    #[test]
    fn parses_refs() {
        let out = "aaa\x1f\x1frefs/heads/main\n\
                   aaa\x1f\x1frefs/remotes/origin/main\n\
                   aaa\x1f\x1frefs/remotes/origin/HEAD\n\
                   ttt\x1fbbb\x1frefs/tags/v1\n\
                   bbb\x1f\x1frefs/notes/x\n";
        let refs = parse_refs(out);
        assert_eq!(
            refs.at("aaa"),
            [Ref::Local("main".into()), Ref::Remote("origin/main".into())]
        );
        // An annotated tag is filed under the commit, not the tag object.
        assert_eq!(refs.at("bbb"), [Ref::Tag("v1".into())]);
        assert!(refs.at("ttt").is_empty());
        assert_eq!(refs.tip(&Ref::Tag("v1".into())), Some("bbb"));
    }

    #[test]
    fn pages_through_history() {
        let dir = temp_repo("history-pages");
        let mut n = 0;
        for i in 0..7 {
            commit(&dir, &format!("c{i}"), &mut n);
        }
        let first = first_page(&dir, &[], 3).unwrap();
        assert_eq!(first.commits.len(), 3);
        assert!(!first.end);
        assert_eq!(first.commits[0].subject, "c6");

        let last = first.commits.last().unwrap().id.clone();
        let second = next_page(&dir, &[], 3, &last, 3).unwrap();
        assert_eq!(second.commits[0].subject, "c3");
        assert!(!second.end);

        let last = second.commits.last().unwrap().id.clone();
        let third = next_page(&dir, &[], 6, &last, 3).unwrap();
        assert_eq!(third.commits.len(), 1);
        assert!(third.end);

        // A new commit shifts positions: the stale cursor is caught.
        commit(&dir, "c7", &mut n);
        assert_eq!(next_page(&dir, &[], 3, &last, 3), Err(PageError::Moved));

        // And the refresh splices only the new commit on top.
        let fresh = first_page(&dir, &[], 3).unwrap();
        let new = new_since(&fresh, &first.commits).unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].subject, "c7");

        // Rewritten history doesn't splice.
        run(&dir, &["reset", "-q", "--hard", "HEAD~3"]);
        commit(&dir, "other", &mut n);
        let fresh = first_page(&dir, &[], 3).unwrap();
        assert_eq!(new_since(&fresh, &first.commits), None);

        let refs = refs(&dir).unwrap();
        assert_eq!(refs.head_branch.as_deref(), Some("main"));
        assert!(refs.is_head(&fresh.commits[0].id));
        assert_eq!(refs.at(&fresh.commits[0].id), [Ref::Local("main".into())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_repo_has_no_history() {
        let dir = temp_repo("history-empty");
        let page = first_page(&dir, &[], 10).unwrap();
        assert!(page.commits.is_empty() && page.end);
        assert!(stashes(&dir).unwrap().is_empty());
        let refs = refs(&dir).unwrap();
        assert_eq!(refs.head, None);
        assert_eq!(refs.head_branch.as_deref(), Some("main"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stashes_hang_off_their_base_and_stay_out_of_the_walk() {
        let dir = temp_repo("history-stash");
        let mut n = 0;
        std::fs::write(dir.join("a.txt"), "1").unwrap();
        run(&dir, &["add", "a.txt"]);
        commit(&dir, "base", &mut n);
        let base = run(&dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("a.txt"), "2").unwrap();
        run(&dir, &["stash", "push", "-q", "-m", "wip one"]);
        std::fs::write(dir.join("a.txt"), "3").unwrap();
        run(&dir, &["stash", "push", "-q", "-m", "wip two"]);

        let stashes = stashes(&dir).unwrap();
        assert_eq!(stashes.len(), 2);
        assert_eq!(stashes[0].name, "stash@{0}");
        assert!(stashes[0].subject.contains("wip two"));
        assert!(stashes.iter().all(|s| s.base == base));

        let page = first_page(&dir, &[], 10).unwrap();
        assert_eq!(page.commits.len(), 1, "stash commits leaked into the walk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extra_tips_are_walked() {
        let dir = temp_repo("history-tips");
        let mut n = 0;
        commit(&dir, "root", &mut n);
        run(&dir, &["checkout", "-q", "--detach"]);
        commit(&dir, "detached", &mut n);
        let lost = run(&dir, &["rev-parse", "HEAD"]);
        run(&dir, &["checkout", "-q", "main"]);
        // Unreachable from any ref now; only an explicit tip finds it.
        assert_eq!(first_page(&dir, &[], 10).unwrap().commits.len(), 1);
        let page = first_page(&dir, std::slice::from_ref(&lost), 10).unwrap();
        assert_eq!(page.commits.len(), 2);
        // Non-hash tips are ignored rather than passed to git.
        assert_eq!(
            first_page(&dir, &["--output=x".to_string()], 10)
                .unwrap()
                .commits
                .len(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
