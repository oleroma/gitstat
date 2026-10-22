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
    /// Extra walk starting points: other worktrees' HEADs.
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
        let (commits, end) =
            match prev.and_then(|p| Some((p, history::new_since(&first, &p.commits)?))) {
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
        Ok(Some(RepoGraph {
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
        self.commits.extend(commits);
        self.end = end;
    }

    /// The stashes made on the commit at `row`.
    pub fn stashes_at(&self, row: usize) -> impl Iterator<Item = &Stash> {
        let id = self.commits.get(row).map(|c| c.id.as_str());
        self.stashes
            .iter()
            .filter(move |s| Some(s.base.as_str()) == id)
    }
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
}
