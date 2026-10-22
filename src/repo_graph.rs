//! Everything the timeline draws for one repo, loaded in one go off the UI
//! thread: refs, header info, stashes, and the history pages laid out.

use crate::graph::{Context, Layout};
use crate::history::{self, Commit, PageError, Stash};
use crate::repo_info::{self, Info};
use std::path::Path;

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
    /// Extra walk starting points: other worktrees' HEADs. `--all` has
    /// walked them too since git 2.16; passing them again costs nothing and
    /// keeps an older git from losing a worktree's detached commits.
    tips: Vec<String>,
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
        // Most checks move no ref: skip the other git calls and the layout.
        if let Some(prev) = prev.filter(|p| p.ctx.refs == refs) {
            if prev.stashes == stashes {
                return Ok(None);
            }
            return Ok(Some(RepoGraph {
                stashes,
                ..prev.clone()
            }));
        }
        let info = repo_info::read(repo, &refs)?;
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
            _ => (first.commits, first.end),
        };
        let ctx = Context {
            default_tip: info.default_tip.clone(),
            unpushed: info.unpushed.clone(),
            unpulled: info.unpulled.clone(),
            refs,
        };
        let mut layout = Layout::new(&ctx);
        layout.push(&commits, &ctx);
        let head_row = find_head(&commits, &ctx, 0);
        Ok(Some(RepoGraph {
            head_row,
            commits,
            layout,
            ctx,
            info,
            stashes,
            end,
            tips,
        }))
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

fn find_head(commits: &[Commit], ctx: &Context, from: usize) -> Option<usize> {
    let head = ctx.refs.head.as_deref()?;
    (from..commits.len()).find(|&i| commits[i].id == head)
}

#[cfg(test)]
mod tests {
    use super::*;
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
