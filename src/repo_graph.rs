//! Everything the timeline draws for one repo, loaded in one go off the UI
//! thread: refs, header info, stashes, and the history pages laid out.

use crate::graph::{Context, Layout, PrColors};
use crate::history::{self, Commit, PageError, Stash};
use crate::repo_info::{self, Info};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Only the Windows UI reads some fields; Linux builds this for its tests.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone)]
pub struct RepoGraph {
    pub commits: Vec<Commit>,
    pub layout: Layout,
    pub ctx: Context,
    pub info: Info,
    pub stashes: Vec<Stash>,
    /// The whole history is loaded.
    pub end: bool,
    /// HEAD's row: `--date-order` puts newer commits on other branches, or
    /// not yet pulled, above it. `None` while unborn or not loaded yet.
    pub head_row: Option<usize>,
    /// The repo's GitHub PRs by branch, which colour their branches. Set
    /// by the timeline, not read from git, so a reload carries it over.
    pub prs: Arc<PrColors>,
    /// Extra walk starting points: other worktrees' HEADs. `--all` has
    /// walked them too since git 2.16; passing them again costs nothing and
    /// keeps an older git from losing a worktree's detached commits.
    tips: Vec<String>,
    /// What `info` was read from besides the refs.
    setup: repo_info::Setup,
    /// The lines checked out in the repo's other worktrees, for the band
    /// drawn under their lanes.
    pub bands: Vec<Band>,
    /// The worktrees besides this repo's folder, whose HEADs `bands` marks:
    /// this one's checkout is HEAD, which has a mark of its own.
    others: Vec<repo_info::Worktree>,
}

/// Another worktree's checkout on the graph: its HEAD's row and the rows
/// down its lane that its branch has of its own, rows `top..=bottom`.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    /// The worktree's folder name.
    pub name: String,
    pub path: PathBuf,
    /// `None` when detached.
    pub branch: Option<String>,
    pub lane: usize,
    pub top: usize,
    pub bottom: usize,
    /// The line runs on past the loaded rows.
    pub open: bool,
}

/// What `more` found.
pub enum More {
    Page(Vec<Commit>, bool),
    /// History moved: load it again from the top.
    Reload,
}

impl RepoGraph {
    /// Reads the repo. With `prev`, commits it already holds are kept and
    /// only new ones are read, unless history was rewritten. `None` when
    /// nothing changed since `prev`.
    pub fn load(
        repo: &Path,
        prev: Option<&RepoGraph>,
        page: usize,
    ) -> Result<Option<RepoGraph>, String> {
        let refs = history::refs(repo)?;
        let stashes = history::stashes(repo).unwrap_or_default();
        let setup = repo_info::setup(repo);
        // Most checks move no ref: skip the other git calls and the layout.
        if let Some(prev) = prev.filter(|p| p.ctx.refs == refs && p.setup == setup) {
            if prev.stashes == stashes {
                return Ok(None);
            }
            return Ok(Some(RepoGraph {
                stashes,
                ..prev.clone()
            }));
        }
        let info = repo_info::read(repo, &refs, &setup)?;
        let tips = info.worktree_tips(refs.head.as_deref());
        let first = history::first_page(repo, &tips, page)?;
        let spliced = prev.and_then(|p| {
            Some((
                p,
                history::new_since(&first, &p.commits, &p.ctx.refs, &refs)?,
            ))
        });
        let (commits, end) = match spliced {
            Some((prev, new)) if !prev.commits.is_empty() => {
                let mut commits = new;
                commits.extend(prev.commits.iter().cloned());
                (commits, prev.end)
            }
            // Rewritten: read as deep as was loaded, so a strip scrolled
            // past the first page still finds the commit it showed.
            _ => match prev.map_or(0, |p| p.commits.len()) {
                deep if deep > page && !first.end => {
                    let all = history::first_page(repo, &tips, deep)?;
                    (all.commits, all.end)
                }
                _ => (first.commits, first.end),
            },
        };
        let ctx = Context {
            default_branch: info.default_branch.clone(),
            default_tip: info.default_tip.clone(),
            unpushed: info.unpushed.clone(),
            unpulled: info.unpulled.clone(),
            refs,
        };
        let mut layout = Layout::new(&ctx);
        layout.push(&commits, &ctx);
        let head_row = find_head(&commits, &ctx, 0);
        let others = other_worktrees(&info, repo, ctx.refs.head.as_deref());
        let bands = bands(&others, &commits, &layout);
        Ok(Some(RepoGraph {
            prs: prev.map(|p| p.prs.clone()).unwrap_or_default(),
            bands,
            others,
            head_row,
            commits,
            layout,
            ctx,
            info,
            stashes,
            end,
            tips,
            setup,
        }))
    }

    /// A graph of a GitHub repo with no clone here, from its recent history
    /// as GitHub has it. Every branch is a remote one (`origin/<name>`), the
    /// default branch stands for HEAD, and there is nothing local: no
    /// worktrees, stashes, or unpushed commits. History ends where the read
    /// stopped.
    pub fn from_remote(h: crate::github::RemoteHistory, web: String) -> RepoGraph {
        use crate::history::{Ref, Refs};
        use crate::repo_info::{Branch, Checkout, Merge};
        let mut refs = Refs::default();
        for (name, tip) in &h.branches {
            refs.insert(tip, Ref::Remote(format!("origin/{name}")));
        }
        let default_tip = h.default_branch.as_ref().and_then(|d| {
            h.branches
                .iter()
                .find(|(name, _)| name == d)
                .map(|(_, tip)| tip.clone())
        });
        refs.head = default_tip.clone();
        refs.head_branch = h.default_branch.clone();
        let by_id: std::collections::HashMap<&str, &Commit> =
            h.commits.iter().map(|c| (c.id.as_str(), c)).collect();
        // What the default branch reaches, within what was read.
        let mut trunk = std::collections::HashSet::new();
        let mut stack: Vec<&str> = default_tip.iter().map(String::as_str).collect();
        while let Some(id) = stack.pop() {
            if trunk.insert(id.to_string())
                && let Some(c) = by_id.get(id)
            {
                stack.extend(c.parents.iter().map(String::as_str));
            }
        }
        // The newest merge down the default branch's first parents.
        let mut last_merge = None;
        let mut at = default_tip.as_deref();
        while let Some(c) = at.and_then(|id| by_id.get(id)) {
            if c.is_merge() {
                last_merge = Some(Merge {
                    id: c.id.clone(),
                    time: c.time,
                    subject: c.subject.clone(),
                });
                break;
            }
            at = c.parents.first().map(String::as_str);
        }
        let branches = h
            .branches
            .iter()
            .filter(|(name, _)| Some(name) != h.default_branch.as_ref())
            .map(|(name, tip)| Branch {
                name: name.clone(),
                local: false,
                remote: true,
                merged: trunk.contains(tip),
                time: by_id.get(tip.as_str()).map_or(0, |c| c.time),
            })
            .collect();
        let default_branch = h.default_branch.as_ref().map(|d| format!("origin/{d}"));
        let info = Info {
            checkout: match &h.default_branch {
                Some(b) => Checkout::Branch(b.clone()),
                None => Checkout::Detached(String::new()),
            },
            worktrees: Vec::new(),
            local_branches: 0,
            remote_branches: h.branches.len(),
            default_branch: default_branch.clone(),
            default_tip: default_tip.clone(),
            last_merge,
            branches,
            unpushed: Default::default(),
            unpulled: Default::default(),
            web: Some(web),
        };
        let ctx = Context {
            default_branch,
            default_tip,
            unpushed: Default::default(),
            unpulled: Default::default(),
            refs,
        };
        let commits = h.commits;
        let mut layout = Layout::new(&ctx);
        layout.push(&commits, &ctx);
        let head_row = find_head(&commits, &ctx, 0);
        RepoGraph {
            prs: Default::default(),
            bands: Vec::new(),
            others: Vec::new(),
            head_row,
            commits,
            layout,
            ctx,
            info,
            stashes: Vec::new(),
            end: true,
            tips: Vec::new(),
            setup: Default::default(),
        }
    }

    /// Reads the page after what is loaded.
    pub fn more(&self, repo: &Path, page: usize) -> Result<More, String> {
        let Some(last) = self.commits.last() else {
            return Ok(More::Page(Vec::new(), true));
        };
        match history::next_page(repo, &self.tips, self.commits.len(), &last.id, page) {
            Ok(p) => Ok(More::Page(p.commits, p.end)),
            Err(PageError::Moved) => Ok(More::Reload),
            Err(PageError::Git(e)) => Err(e),
        }
    }

    pub fn append(&mut self, commits: Vec<Commit>, end: bool) {
        self.layout.push(&commits, &self.ctx);
        let from = self.commits.len();
        self.commits.extend(commits);
        self.end = end;
        if self.head_row.is_none() {
            self.head_row = find_head(&self.commits, &self.ctx, from);
        }
        // A band may reach into the new rows, or its HEAD only now be loaded.
        self.bands = bands(&self.others, &self.commits, &self.layout);
    }

    /// The other worktrees whose HEAD is the commit at `row`.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn bands_at(&self, row: usize) -> impl Iterator<Item = &Band> {
        self.bands.iter().filter(move |b| b.top == row)
    }

    /// The row a strip rests on: HEAD's, or the newest without one.
    pub fn home(&self) -> f32 {
        self.head_row.unwrap_or(0) as f32
    }

    /// Where a strip that showed row `at` of `old` goes in this reload of
    /// it, in rows: home if it was home, so it follows HEAD; else on the
    /// same commit, so new commits don't slide what was being looked at.
    pub fn carry_scroll(&self, old: &RepoGraph, at: f32) -> f32 {
        let last = self.commits.len().saturating_sub(1) as f32;
        if old.commits.is_empty() || (at - old.home()).abs() < 1e-3 {
            return self.home();
        }
        let row = (at.round().max(0.0) as usize).min(old.commits.len() - 1);
        let id = &old.commits[row].id;
        match self.commits.iter().position(|c| &c.id == id) {
            Some(n) => (n as f32 + at - row as f32).clamp(0.0, last),
            // Rewritten away: stay put.
            None => at.clamp(0.0, last),
        }
    }

    /// The stashes made on the commit at `row`.
    pub fn stashes_at(&self, row: usize) -> impl Iterator<Item = &Stash> {
        let id = self.commits.get(row).map(|c| c.id.as_str());
        self.stashes
            .iter()
            .filter(move |s| Some(s.base.as_str()) == id)
    }
}

/// The repo's worktrees other than `repo` itself that have a commit
/// checked out. When `repo` can't be resolved, the worktree on HEAD's own
/// commit stands for it.
fn other_worktrees(info: &Info, repo: &Path, head: Option<&str>) -> Vec<repo_info::Worktree> {
    let here = std::fs::canonicalize(repo).ok();
    info.worktrees
        .iter()
        .filter(|w| !w.bare && !w.prunable && w.head.is_some())
        .filter(|w| match &here {
            Some(here) => std::fs::canonicalize(&w.path).map_or(true, |p| &p != here),
            None => w.head.as_deref() != head,
        })
        .cloned()
        .collect()
}

/// The bands of the worktrees whose HEAD is loaded.
fn bands(others: &[repo_info::Worktree], commits: &[Commit], layout: &Layout) -> Vec<Band> {
    others
        .iter()
        .filter_map(|w| {
            let id = w.head.as_deref()?;
            let top = commits.iter().position(|c| c.id == id)?;
            let (bottom, open) = layout.lane_run(top);
            Some(Band {
                name: w.path.file_name().map_or_else(
                    || w.path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                path: w.path.clone(),
                branch: w.branch.clone(),
                lane: layout.nodes[top].lane,
                top,
                bottom,
                open,
            })
        })
        .collect()
}

fn find_head(commits: &[Commit], ctx: &Context, from: usize) -> Option<usize> {
    let head = ctx.refs.head.as_deref()?;
    (from..commits.len()).find(|&i| commits[i].id == head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_graph_marks_merged_and_hanging_branches() {
        let c = |id: &str, time: i64, parents: &[&str]| Commit {
            id: id.into(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: "a".into(),
            email: "a@x".into(),
            time,
            subject: id.into(),
        };
        let h = crate::github::RemoteHistory {
            default_branch: Some("main".into()),
            branches: vec![
                ("main".into(), "m2".into()),
                ("done".into(), "d1".into()),
                ("wip".into(), "w1".into()),
            ],
            commits: vec![
                c("w1", 5, &["m1"]),
                c("m2", 4, &["m1", "d1"]),
                c("d1", 3, &["root"]),
                c("m1", 2, &["root"]),
                c("root", 1, &[]),
            ],
        };
        let g = RepoGraph::from_remote(h, "https://github.com/o/r".into());
        assert_eq!(g.info.default_branch.as_deref(), Some("origin/main"));
        assert_eq!(g.head_row, Some(1));
        assert!(g.end);
        let merged: Vec<(&str, bool)> = g
            .info
            .branches
            .iter()
            .map(|b| (b.name.as_str(), b.merged))
            .collect();
        assert_eq!(merged, [("done", true), ("wip", false)]);
        assert_eq!(g.info.last_merge.as_ref().unwrap().id, "m2");
        assert_eq!(g.layout.nodes.len(), 5);
    }
    use crate::history::tests::{commit, run, temp_repo};

    #[test]
    fn loads_pages_and_splices_refreshes() {
        let dir = temp_repo("repo-graph");
        let mut n = 0;
        for i in 0..5 {
            commit(&dir, &format!("c{i}"), &mut n);
        }
        let mut g = RepoGraph::load(&dir, None, 2).unwrap().unwrap();
        assert_eq!(g.commits.len(), 2);
        assert!(!g.end);
        match g.more(&dir, 2).unwrap() {
            More::Page(c, end) => g.append(c, end),
            More::Reload => panic!("history didn't move"),
        }
        assert_eq!(g.commits.len(), 4);
        assert_eq!(g.layout.nodes.len(), 4);

        commit(&dir, "c5", &mut n);
        let g2 = RepoGraph::load(&dir, Some(&g), 2).unwrap().unwrap();
        // The new commit on top of the four already loaded.
        assert_eq!(g2.commits.len(), 5);
        assert_eq!(g2.commits[0].subject, "c5");
        assert!(g2.ctx.refs.is_head(&g2.commits[0].id));
        assert_eq!(g2.layout.nodes[0].lane, 0);

        // The old graph's cursor is stale now.
        assert!(matches!(g.more(&dir, 2).unwrap(), More::Reload));

        run(&dir, &["stash", "list"]);
        assert_eq!(g2.stashes_at(0).count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reuses_the_graph_until_a_ref_or_stash_moves() {
        let dir = temp_repo("repo-graph-same");
        let mut n = 0;
        std::fs::write(dir.join("f"), "a").unwrap();
        run(&dir, &["add", "f"]);
        for i in 0..3 {
            commit(&dir, &format!("c{i}"), &mut n);
        }
        let g = RepoGraph::load(&dir, None, 10).unwrap().unwrap();
        assert!(RepoGraph::load(&dir, Some(&g), 10).unwrap().is_none());

        // A tag moves no branch or HEAD, but is a ref the strip draws.
        run(&dir, &["tag", "v1"]);
        let g = RepoGraph::load(&dir, Some(&g), 10).unwrap().unwrap();
        assert_eq!(g.ctx.refs.at(&g.commits[0].id).len(), 2);
        assert!(RepoGraph::load(&dir, Some(&g), 10).unwrap().is_none());

        // A stash moves only refs/stash.
        std::fs::write(dir.join("f"), "b").unwrap();
        run(&dir, &["stash"]);
        let g2 = RepoGraph::load(&dir, Some(&g), 10).unwrap().unwrap();
        assert_eq!(g2.stashes_at(0).count(), 1);
        assert_eq!(g2.commits, g.commits);

        // A worktree, or origin/HEAD, changes the header with no ref moved.
        let wt = std::env::temp_dir().join(format!("gitstat-rg-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        run(
            &dir,
            &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
        );
        let g3 = RepoGraph::load(&dir, Some(&g2), 10).unwrap().unwrap();
        assert_eq!(g3.info.worktrees.len(), 2);
        assert!(RepoGraph::load(&dir, Some(&g3), 10).unwrap().is_none());
        run(&dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        run(&dir, &["update-ref", "refs/remotes/origin/dev", "HEAD~1"]);
        let g4 = RepoGraph::load(&dir, Some(&g3), 10).unwrap().unwrap();
        assert_eq!(g4.info.default_branch.as_deref(), Some("origin/main"));
        run(
            &dir,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/dev",
            ],
        );
        let g5 = RepoGraph::load(&dir, Some(&g4), 10).unwrap().unwrap();
        assert_eq!(g5.info.default_branch.as_deref(), Some("origin/dev"));
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bands_mark_other_worktrees_lines() {
        let dir = temp_repo("repo-graph-bands");
        let mut n = 0;
        for i in 0..3 {
            commit(&dir, &format!("c{i}"), &mut n);
        }
        let wt = std::env::temp_dir().join(format!("gitstat-rg-band-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        run(
            &dir,
            &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
        );
        commit(&wt, "f1", &mut n);
        commit(&wt, "f2", &mut n);
        commit(&dir, "c3", &mut n);

        let g = RepoGraph::load(&dir, None, 10).unwrap().unwrap();
        let subject = |row: usize| g.commits[row].subject.as_str();
        // Only the other worktree: this one's checkout is HEAD.
        assert_eq!(g.bands.len(), 1);
        let b = &g.bands[0];
        assert_eq!(b.branch.as_deref(), Some("feat"));
        assert_eq!(subject(b.top), "f2");
        assert_eq!(subject(b.bottom), "f1");
        assert!(!b.open);
        assert_ne!(b.lane, 0);
        assert_eq!(g.bands_at(b.top).count(), 1);

        // Seen from the worktree, the main folder's line is the band.
        let g = RepoGraph::load(&wt, None, 10).unwrap().unwrap();
        assert_eq!(g.bands.len(), 1);
        assert_eq!(g.bands[0].branch.as_deref(), Some("main"));
        assert_eq!(g.commits[g.bands[0].top].subject, "c3");

        // Loaded a page at a time, the band reaches on as rows come.
        let mut g = RepoGraph::load(&dir, None, 2).unwrap().unwrap();
        let b = g.bands[0].clone();
        assert!(b.open && b.bottom == b.top);
        match g.more(&dir, 10).unwrap() {
            More::Page(c, end) => g.append(c, end),
            More::Reload => panic!("history didn't move"),
        }
        assert_eq!(g.commits[g.bands[0].bottom].subject, "f1");
        assert!(!g.bands[0].open);

        run(
            &dir,
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
        let _ = std::fs::remove_dir_all(&wt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_head_below_newer_commits() {
        let dir = temp_repo("repo-graph-head");
        let mut n = 0;
        for i in 0..3 {
            commit(&dir, &format!("c{i}"), &mut n);
        }
        // A newer commit on another branch sorts above HEAD.
        run(&dir, &["checkout", "-q", "-b", "other"]);
        commit(&dir, "o", &mut n);
        run(&dir, &["checkout", "-q", "main"]);
        let g = RepoGraph::load(&dir, None, 10).unwrap().unwrap();
        assert_eq!(g.commits[0].subject, "o");
        assert_eq!(g.head_row, Some(1));
        assert_eq!(g.home(), 1.0);

        // Detached, HEAD is the commit checked out.
        run(&dir, &["checkout", "-q", "--detach", "main~1"]);
        let g = RepoGraph::load(&dir, Some(&g), 10).unwrap().unwrap();
        assert_eq!(g.commits[g.head_row.unwrap()].subject, "c1");

        // Found on a later page when the first holds only newer commits.
        let mut g = RepoGraph::load(&dir, None, 2).unwrap().unwrap();
        assert_eq!(g.head_row, None);
        assert_eq!(g.home(), 0.0);
        match g.more(&dir, 2).unwrap() {
            More::Page(c, end) => g.append(c, end),
            More::Reload => panic!("history didn't move"),
        }
        assert_eq!(g.head_row, Some(2));

        // Unborn: other branches' commits, but no HEAD among them.
        run(&dir, &["checkout", "-q", "--orphan", "fresh"]);
        let g = RepoGraph::load(&dir, None, 10).unwrap().unwrap();
        assert!(!g.commits.is_empty());
        assert_eq!(g.head_row, None);
        assert_eq!(g.home(), 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scroll_holds_past_the_first_page_when_history_is_rewritten() {
        let dir = temp_repo("repo-graph-deep");
        let mut n = 0;
        for i in 0..8 {
            commit(&dir, &format!("c{i}"), &mut n);
        }
        let mut g = RepoGraph::load(&dir, None, 2).unwrap().unwrap();
        for _ in 0..2 {
            match g.more(&dir, 2).unwrap() {
                More::Page(c, end) => g.append(c, end),
                More::Reload => panic!("history didn't move"),
            }
        }
        assert_eq!(g.commits.len(), 6);
        // Amending HEAD drops the newest loaded commit: no splice.
        run(
            &dir,
            &["commit", "-q", "--amend", "--allow-empty", "-m", "c7'"],
        );
        let g2 = RepoGraph::load(&dir, Some(&g), 2).unwrap().unwrap();
        assert_eq!(g2.commits.len(), 6);
        assert!(!g2.end);
        let row = g2.carry_scroll(&g, 4.0);
        assert_eq!(g2.commits[row as usize].id, g.commits[4].id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_repo_rests_on_row_zero() {
        let dir = temp_repo("repo-graph-empty");
        let g = RepoGraph::load(&dir, None, 10).unwrap().unwrap();
        assert!(g.commits.is_empty());
        assert_eq!(g.head_row, None);
        assert_eq!(g.carry_scroll(&g, 0.0), 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scroll_holds_its_commit_or_follows_head() {
        let dir = temp_repo("repo-graph-scroll");
        let mut n = 0;
        for i in 0..6 {
            commit(&dir, &format!("c{i}"), &mut n);
        }
        let g = RepoGraph::load(&dir, None, 10).unwrap().unwrap();
        commit(&dir, "c6", &mut n);
        let g2 = RepoGraph::load(&dir, Some(&g), 10).unwrap().unwrap();
        // Home follows HEAD to the new commit.
        assert_eq!(g2.carry_scroll(&g, 0.0), 0.0);
        // Scrolled away, the same commit stays in view, fraction and all.
        assert_eq!(g.commits[3].subject, "c2");
        assert_eq!(g2.carry_scroll(&g, 3.25), 4.25);
        assert_eq!(g2.commits[4].subject, "c2");

        // A newer commit elsewhere puts HEAD below row 0; home follows it.
        run(&dir, &["checkout", "-q", "-b", "other"]);
        commit(&dir, "o", &mut n);
        run(&dir, &["checkout", "-q", "main"]);
        let g3 = RepoGraph::load(&dir, Some(&g2), 10).unwrap().unwrap();
        assert_eq!(g3.carry_scroll(&g2, 0.0), 1.0);
        assert_eq!(g3.carry_scroll(&g2, 4.0), 5.0);

        // Rewritten away: the scroll stays where it was.
        run(&dir, &["reset", "-q", "--hard", "main~3"]);
        run(&dir, &["branch", "-q", "-D", "other"]);
        commit(&dir, "r", &mut n);
        let g4 = RepoGraph::load(&dir, Some(&g3), 10).unwrap().unwrap();
        assert_eq!(g4.carry_scroll(&g3, 2.0), 2.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
