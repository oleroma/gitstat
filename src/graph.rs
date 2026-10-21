//! Lays a repo's history out as a graph: a lane per line of development,
//! a row per commit, and the noodles between them.
//!
//! Rows are commits in `history` order, newest first, so every commit comes
//! after its children. Lanes are assigned in one pass:
//!
//! - A commit takes the lowest lane already waiting for it, or a free one
//!   if it is a branch tip nothing loaded points to. Lane 0 is kept for
//!   HEAD, so the checked-out line is always the straight one.
//! - Its first parent continues in its lane. Every other parent joins a
//!   lane already waiting for that commit, or opens a free one.
//! - Two lanes waiting for the same commit both run until it, and meet
//!   there: branches converge on the dot, as in GitKraken, rather than
//!   jumping lanes early.
//!
//! The layout is built page by page; what is still waiting at the end of a
//! page carries over to the next. Nothing here knows about pixels: rows
//! and lanes are positions that the view maps to the strip (rows run right
//! to left) or the column (rows run down), or anything in between.

// Wired into the tray and window views in a later milestone.
#![allow(dead_code)]

use crate::history::{Commit, Ref, Refs};
use std::collections::{HashMap, HashSet};

/// A row and lane. Fractional when a noodle bends between lanes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pos {
    pub row: f32,
    pub lane: f32,
}

fn pos(row: usize, lane: usize) -> Pos {
    Pos {
        row: row as f32,
        lane: lane as f32,
    }
}

/// What a commit's colour says about it, most important first. The PR
/// states (open, draft, closed) are applied on top by branch name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// Reachable from HEAD but not from its upstream.
    Unpushed,
    /// Reachable from the upstream but not from HEAD.
    Unpulled,
    /// On the default branch's first-parent line.
    Trunk,
    /// Merged into the default branch.
    Merged,
    /// On a branch of its own, coloured by a hue hashed from its name.
    Branch,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub lane: usize,
    pub class: Class,
    /// The branch this commit belongs to: its first-parent child's, or
    /// for a tip, the branch pointing at it. Index into
    /// [`Layout::branches`].
    pub branch: Option<usize>,
    pub merge: bool,
}

/// A noodle from a child down to one of its parents. It leaves the child,
/// bends into lane `via`, runs along it, and bends into the parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub child: (usize, usize),
    pub via: usize,
    /// `None` while the parent hasn't been loaded: the noodle runs on past
    /// the last row.
    pub parent: Option<(usize, usize)>,
    /// The child's first parent. Its colour is the child's; a merged-in
    /// parent's noodle takes the parent's colour.
    pub first: bool,
}

/// A piece of a noodle, for drawing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Straight(Pos, Pos),
    /// A bend between lanes, drawn as an S-curve that leaves and enters
    /// along the flow of the rows.
    Curve(Pos, Pos),
}

impl Edge {
    /// The noodle as straight runs and bends. An edge still waiting for its
    /// parent ends at `last_row`.
    pub fn segments(&self, last_row: usize) -> Vec<Seg> {
        let (r1, l1) = self.child;
        let (r2, l2) = self.parent.unwrap_or((last_row.max(r1 + 1), self.via));
        let v = self.via;
        if r2 <= r1 + 1 {
            return if l1 == l2 {
                vec![Seg::Straight(pos(r1, l1), pos(r2, l2))]
            } else {
                vec![Seg::Curve(pos(r1, l1), pos(r2, l2))]
            };
        }
        let mut segs = Vec::with_capacity(3);
        let mut at = pos(r1, l1);
        if l1 != v {
            let next = pos(r1 + 1, v);
            segs.push(Seg::Curve(at, next));
            at = next;
        }
        let bend = if l2 != v { r2 - 1 } else { r2 };
        if (at.row as usize) < bend {
            let next = pos(bend, v);
            segs.push(Seg::Straight(at, next));
            at = next;
        }
        if l2 != v {
            segs.push(Seg::Curve(at, pos(r2, l2)));
        }
        segs
    }
}

/// What the layout needs to know about the repo besides its commits.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub refs: Refs,
    /// The default branch's tip, e.g. `origin/main`'s commit.
    pub default_tip: Option<String>,
    pub unpushed: HashSet<String>,
    pub unpulled: HashSet<String>,
}

/// A lane waiting for a commit: the noodles that will end on it.
#[derive(Clone, Debug)]
struct Slot {
    target: String,
    sources: Vec<Source>,
}

#[derive(Clone, Copy, Debug)]
struct Source {
    child: (usize, usize),
    first: bool,
    /// The child's branch, which a first parent inherits.
    branch: Option<usize>,
}

#[derive(Debug, Default)]
pub struct Layout {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Branch names that nodes point into.
    pub branches: Vec<String>,
    /// The most lanes any row used.
    pub width: usize,
    rows: HashMap<String, usize>,
    lanes: Vec<Option<Slot>>,
    /// Commits still to come that are on the default branch's first-parent
    /// line, or merged into it.
    trunk: HashSet<String>,
    merged: HashSet<String>,
    branch_ids: HashMap<String, usize>,
    /// Edges waiting for a parent not loaded yet, by edge index.
    open: HashMap<usize, String>,
}

impl Layout {
    pub fn new(ctx: &Context) -> Self {
        let mut layout = Layout::default();
        if let Some(head) = &ctx.refs.head {
            layout.lanes.push(Some(Slot {
                target: head.clone(),
                sources: Vec::new(),
            }));
        }
        if let Some(tip) = &ctx.default_tip {
            layout.trunk.insert(tip.clone());
        }
        layout
    }

    /// Adds the next page of commits, newest first.
    pub fn push(&mut self, commits: &[Commit], ctx: &Context) {
        for commit in commits {
            self.add(commit, ctx);
        }
    }

    pub fn row(&self, id: &str) -> Option<usize> {
        self.rows.get(id).copied()
    }

    /// Edges still waiting for a parent below the last loaded row.
    pub fn open_edges(&self) -> impl Iterator<Item = &Edge> {
        self.open.keys().map(|&i| &self.edges[i])
    }

    fn add(&mut self, commit: &Commit, ctx: &Context) {
        let row = self.nodes.len();
        self.rows.insert(commit.id.clone(), row);

        let waiting: Vec<usize> = (0..self.lanes.len())
            .filter(|&i| matches!(&self.lanes[i], Some(s) if s.target == commit.id))
            .collect();
        let lane = match waiting.first() {
            Some(&lane) => lane,
            None => self.free_lane(),
        };
        // The branch of the first-parent child whose line continues here,
        // so where branches meet, the one in the lower lane (HEAD's, or the
        // older one) keeps the commit.
        let inherited = self.lanes[lane]
            .as_ref()
            .and_then(|slot| slot.sources.iter().find(|s| s.first).and_then(|s| s.branch));
        for &i in &waiting {
            let slot = self.lanes[i].take().expect("waiting lane");
            for s in slot.sources {
                self.close(s.child, i, (row, lane), s.first, &commit.id);
            }
        }

        let (class, branch) = self.classify(commit, inherited, ctx);
        self.nodes.push(Node {
            lane,
            class,
            branch,
            merge: commit.is_merge(),
        });

        for (i, parent) in commit.parents.iter().enumerate() {
            let first = i == 0;
            let via = if first {
                self.lanes[lane] = Some(Slot {
                    target: parent.clone(),
                    sources: Vec::new(),
                });
                lane
            } else {
                match self
                    .lanes
                    .iter()
                    .position(|s| matches!(s, Some(s) if &s.target == parent))
                {
                    Some(j) => j,
                    None => {
                        let j = self.free_lane();
                        self.lanes[j] = Some(Slot {
                            target: parent.clone(),
                            sources: Vec::new(),
                        });
                        j
                    }
                }
            };
            let slot = self.lanes[via].as_mut().expect("parent lane");
            slot.sources.push(Source {
                child: (row, lane),
                first,
                branch,
            });
            // Open until the parent is loaded.
            self.open.insert(self.edges.len(), parent.clone());
            self.edges.push(Edge {
                child: (row, lane),
                via,
                parent: None,
                first,
            });
        }
        while matches!(self.lanes.last(), Some(None)) {
            self.lanes.pop();
        }
        self.width = self.width.max(self.lanes.len()).max(lane + 1);
    }

    /// Ends the open edge from `child` through lane `via` at the parent.
    fn close(
        &mut self,
        child: (usize, usize),
        via: usize,
        at: (usize, usize),
        first: bool,
        id: &str,
    ) {
        let found = self.open.iter().find_map(|(&i, target)| {
            let e = &self.edges[i];
            (target == id && e.child == child && e.via == via && e.first == first).then_some(i)
        });
        if let Some(i) = found {
            self.open.remove(&i);
            self.edges[i].parent = Some(at);
        }
    }

    fn free_lane(&mut self) -> usize {
        match self.lanes.iter().position(Option::is_none) {
            Some(i) => i,
            None => {
                self.lanes.push(None);
                self.lanes.len() - 1
            }
        }
    }

    fn classify(
        &mut self,
        commit: &Commit,
        inherited: Option<usize>,
        ctx: &Context,
    ) -> (Class, Option<usize>) {
        let id = &commit.id;
        let on_trunk = self.trunk.remove(id);
        let merged = on_trunk || self.merged.remove(id);
        if let Some(first) = commit.parents.first()
            && on_trunk
        {
            self.trunk.insert(first.clone());
        }
        if merged {
            self.merged.extend(commit.parents.iter().cloned());
        }

        // The first-parent child's branch carries down; a commit nothing
        // carries to takes the branch pointing at it, local before remote.
        let refs = ctx.refs.at(id);
        let own = refs
            .iter()
            .find(|r| matches!(r, Ref::Local(_)))
            .or_else(|| refs.iter().find(|r| matches!(r, Ref::Remote(_))))
            .map(|r| self.branch_id(r.name()));
        let branch = inherited.or(own);

        let class = if ctx.unpushed.contains(id) {
            Class::Unpushed
        } else if ctx.unpulled.contains(id) {
            Class::Unpulled
        } else if on_trunk {
            Class::Trunk
        } else if merged {
            Class::Merged
        } else {
            Class::Branch
        };
        (class, branch)
    }

    fn branch_id(&mut self, name: &str) -> usize {
        if let Some(&i) = self.branch_ids.get(name) {
            return i;
        }
        self.branches.push(name.to_string());
        self.branch_ids
            .insert(name.to_string(), self.branches.len() - 1);
        self.branches.len() - 1
    }
}

/// A stable hue index for a branch name, `0..n`: the same branch keeps its
/// colour across runs and machines. FNV-1a.
pub fn hue(name: &str, n: usize) -> usize {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    (h as usize) % n.max(1)
}

/// A point in the view, in pixels.
pub type Point = (f32, f32);

/// The control points of a noodle from `a` to `b` whose ends leave and
/// enter along `flow`, the unit direction rows advance in: like Blender's
/// geometry-node wires, a long hop gets a long lazy curve and a short one a
/// tight S. Works for any `flow`, so the curves stay smooth while the graph
/// turns from the strip to the column.
pub fn noodle(a: Point, b: Point, flow: Point) -> [Point; 4] {
    const K: f32 = 0.5;
    let along = ((b.0 - a.0) * flow.0 + (b.1 - a.1) * flow.1).abs() * K;
    [
        a,
        (a.0 + flow.0 * along, a.1 + flow.1 * along),
        (b.0 - flow.0 * along, b.1 - flow.1 * along),
        b,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.to_string(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            author: String::new(),
            email: String::new(),
            time: 0,
            subject: String::new(),
        }
    }

    fn ctx(head: &str, refs: &[(&str, Ref)]) -> Context {
        let mut ctx = Context::default();
        ctx.refs.head = Some(head.to_string());
        for (id, r) in refs {
            ctx.refs
                .by_commit
                .entry(id.to_string())
                .or_default()
                .push(r.clone());
        }
        ctx
    }

    fn layout(commits: &[Commit], ctx: &Context) -> Layout {
        let mut l = Layout::new(ctx);
        l.push(commits, ctx);
        l
    }

    fn lanes(l: &Layout) -> Vec<usize> {
        l.nodes.iter().map(|n| n.lane).collect()
    }

    fn edge(l: &Layout, child: usize, parent: usize) -> Edge {
        *l.edges
            .iter()
            .find(|e| e.child.0 == child && e.parent.map(|p| p.0) == Some(parent))
            .unwrap_or_else(|| panic!("no edge {child} -> {parent}"))
    }

    #[test]
    fn linear_history_is_one_straight_lane() {
        let commits = [c("c", &["b"]), c("b", &["a"]), c("a", &[])];
        let l = layout(&commits, &ctx("c", &[]));
        assert_eq!(lanes(&l), [0, 0, 0]);
        assert_eq!(l.width, 1);
        assert_eq!(l.edges.len(), 2);
        assert!(l.edges.iter().all(|e| e.via == 0 && e.first));
        assert_eq!(l.open_edges().count(), 0);
    }

    #[test]
    fn branch_and_merge() {
        // m merges f into the trunk; f branched from a.
        //   m       row 0  lane 0
        //   | f     row 1  lane 1
        //   b |     row 2  lane 0
        //   |/
        //   a       row 3  lane 0
        let commits = [
            c("m", &["b", "f"]),
            c("f", &["a"]),
            c("b", &["a"]),
            c("a", &[]),
        ];
        let l = layout(&commits, &ctx("m", &[]));
        assert_eq!(lanes(&l), [0, 1, 0, 0]);
        assert!(l.nodes[0].merge);
        let merge_in = edge(&l, 0, 1);
        assert_eq!((merge_in.via, merge_in.first), (1, false));
        // f's line runs in lane 1 and bends into a at the end.
        let f_down = edge(&l, 1, 3);
        assert_eq!(f_down.via, 1);
        assert_eq!(f_down.parent, Some((3, 0)));
        assert_eq!(
            f_down.segments(4),
            vec![
                Seg::Straight(pos(1, 1), pos(2, 1)),
                Seg::Curve(pos(2, 1), pos(3, 0)),
            ]
        );
        assert_eq!(l.width, 2);
    }

    #[test]
    fn head_keeps_lane_zero() {
        // A newer branch tip comes before HEAD in date order.
        let commits = [c("t", &["a"]), c("h", &["a"]), c("a", &[])];
        let l = layout(&commits, &ctx("h", &[]));
        assert_eq!(lanes(&l), [1, 0, 0]);
    }

    #[test]
    fn lanes_are_reused_after_a_branch_ends() {
        let commits = [
            c("m2", &["m1", "y"]),
            c("y", &["m1"]),
            c("m1", &["a", "x"]),
            c("x", &["a"]),
            c("a", &[]),
        ];
        let l = layout(&commits, &ctx("m2", &[]));
        assert_eq!(lanes(&l), [0, 1, 0, 1, 0]);
        assert_eq!(l.width, 2);
    }

    #[test]
    fn octopus_merge_opens_a_lane_per_parent() {
        let commits = [
            c("o", &["a", "b", "d"]),
            c("d", &["r"]),
            c("b", &["r"]),
            c("a", &["r"]),
            c("r", &[]),
        ];
        let l = layout(&commits, &ctx("o", &[]));
        assert_eq!(l.nodes[0].lane, 0);
        assert_eq!(l.width, 3);
        assert_eq!(l.nodes[4].lane, 0);
        // All three lines meet on the root.
        assert_eq!(
            l.edges
                .iter()
                .filter(|e| e.parent.map(|p| p.0) == Some(4))
                .count(),
            3
        );
    }

    #[test]
    fn criss_cross_merges_close_every_edge() {
        let commits = [
            c("m1", &["a", "b"]),
            c("m2", &["b", "a"]),
            c("b", &["r"]),
            c("a", &["r"]),
            c("r", &[]),
        ];
        let l = layout(&commits, &ctx("m1", &[]));
        assert_eq!(l.open_edges().count(), 0);
        assert!(l.edges.iter().all(|e| e.parent.is_some()));
        // a is waited for in two lanes; it takes the lower one.
        assert_eq!(l.nodes[3].lane, 0);
    }

    #[test]
    fn pages_carry_open_edges_over() {
        let commits = [c("c", &["b"]), c("b", &["a"]), c("a", &[])];
        let ctx = ctx("c", &[]);
        let mut l = Layout::new(&ctx);
        l.push(&commits[..2], &ctx);
        assert_eq!(l.open_edges().count(), 1);
        let open = *l.open_edges().next().unwrap();
        assert_eq!(open.segments(2), vec![Seg::Straight(pos(1, 0), pos(2, 0))]);
        l.push(&commits[2..], &ctx);
        assert_eq!(l.open_edges().count(), 0);
        assert_eq!(edge(&l, 1, 2).parent, Some((2, 0)));
    }

    #[test]
    fn classes_and_branches() {
        //   u        unpushed, on main (HEAD)
        //   m        merges feature into main
        //   | f      feature
        //   t |      trunk
        //   |/
        //   r
        //       o    another branch, unmerged, off r
        let commits = [
            c("o", &["r"]),
            c("u", &["m"]),
            c("m", &["t", "f"]),
            c("f", &["r"]),
            c("t", &["r"]),
            c("r", &[]),
        ];
        let mut ctx = ctx(
            "u",
            &[
                ("u", Ref::Local("main".into())),
                ("m", Ref::Remote("origin/main".into())),
                ("o", Ref::Local("topic".into())),
            ],
        );
        ctx.default_tip = Some("m".into());
        ctx.unpushed.insert("u".into());
        let l = layout(&commits, &ctx);
        let class = |row: usize| l.nodes[row].class;
        let branch = |row: usize| l.nodes[row].branch.map(|b| l.branches[b].as_str());
        assert_eq!(class(0), Class::Branch);
        assert_eq!(branch(0), Some("topic"));
        assert_eq!(class(1), Class::Unpushed);
        assert_eq!(class(2), Class::Trunk);
        assert_eq!(class(3), Class::Merged);
        assert_eq!(class(4), Class::Trunk);
        assert_eq!(class(5), Class::Trunk);
        // main's tip names the first-parent line below it.
        assert_eq!(branch(1), Some("main"));
        assert_eq!(branch(4), Some("main"));
        // The merged-in commit has no branch left pointing at it.
        assert_eq!(branch(3), None);
        // Where topic and main meet, HEAD's lane keeps the commit.
        assert_eq!(branch(5), Some("main"));
    }

    #[test]
    fn segments_bend_out_and_back() {
        let e = Edge {
            child: (0, 0),
            via: 2,
            parent: Some((5, 1)),
            first: false,
        };
        assert_eq!(
            e.segments(10),
            vec![
                Seg::Curve(pos(0, 0), pos(1, 2)),
                Seg::Straight(pos(1, 2), pos(4, 2)),
                Seg::Curve(pos(4, 2), pos(5, 1)),
            ]
        );
        let hop = Edge {
            child: (3, 1),
            via: 1,
            parent: Some((4, 0)),
            first: true,
        };
        assert_eq!(hop.segments(10), vec![Seg::Curve(pos(3, 1), pos(4, 0))]);
    }

    #[test]
    fn noodle_controls_follow_the_flow() {
        // Strip: rows run right to left.
        let [_, c1, c2, _] = noodle((100.0, 0.0), (40.0, 10.0), (-1.0, 0.0));
        assert_eq!(c1, (70.0, 0.0));
        assert_eq!(c2, (70.0, 10.0));
        // Column: rows run down.
        let [_, c1, c2, _] = noodle((0.0, 0.0), (10.0, 20.0), (0.0, 1.0));
        assert_eq!(c1, (0.0, 10.0));
        assert_eq!(c2, (10.0, 10.0));
    }

    #[test]
    fn hues_are_stable_and_in_range() {
        assert_eq!(hue("main", 6), hue("main", 6));
        assert!((0..50).all(|i| hue(&format!("b{i}"), 6) < 6));
        assert_eq!(hue("x", 0), 0);
    }

    #[test]
    fn lays_out_a_large_history_quickly() {
        // 100k commits: a trunk with a short branch merged every 10 commits.
        let mut commits = Vec::new();
        let n = 100_000;
        for i in (0..n).rev() {
            let id = format!("t{i}");
            let parent = if i == 0 {
                vec![]
            } else {
                vec![format!("t{}", i - 1)]
            };
            if i % 10 == 5 && i > 10 {
                commits.push(c(&id, &[&parent[0], &format!("b{i}")]));
                // The branch commit sits between the merge and its base.
                commits.push(c(&format!("b{i}"), &[&format!("t{}", i - 3)]));
            } else {
                let p: Vec<&str> = parent.iter().map(String::as_str).collect();
                commits.push(c(&id, &p));
            }
        }
        let ctx = ctx("t99999", &[]);
        let start = std::time::Instant::now();
        let mut l = Layout::new(&ctx);
        for page in commits.chunks(300) {
            l.push(page, &ctx);
        }
        assert_eq!(l.nodes.len(), commits.len());
        assert_eq!(l.open_edges().count(), 0);
        assert!(l.width <= 3, "width {}", l.width);
        // Generous, for debug builds on slow CI runners.
        assert!(start.elapsed().as_secs() < 20, "{:?}", start.elapsed());
    }
}
