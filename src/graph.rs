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
    /// parent ends at `last_row`. At most three pieces, kept off the heap
    /// since every visible edge is cut up every frame.
    pub fn segments(&self, last_row: usize) -> impl Iterator<Item = Seg> {
        let (r1, l1) = self.child;
        let (r2, l2) = self.parent.unwrap_or((last_row.max(r1 + 1), self.via));
        let v = self.via;
        let mut segs = [None; 3];
        if r2 <= r1 + 1 {
            segs[0] = Some(if l1 == l2 {
                Seg::Straight(pos(r1, l1), pos(r2, l2))
            } else {
                Seg::Curve(pos(r1, l1), pos(r2, l2))
            });
            return segs.into_iter().flatten();
        }
        let mut at = pos(r1, l1);
        if l1 != v {
            let next = pos(r1 + 1, v);
            segs[0] = Some(Seg::Curve(at, next));
            at = next;
        }
        let bend = if l2 != v { r2 - 1 } else { r2 };
        if (at.row as usize) < bend {
            let next = pos(bend, v);
            segs[1] = Some(Seg::Straight(at, next));
            at = next;
        }
        if l2 != v {
            segs[2] = Some(Seg::Curve(at, pos(r2, l2)));
        }
        segs.into_iter().flatten()
    }
}

/// What the layout needs to know about the repo besides its commits.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub refs: Refs,
    /// The default branch, `origin/main` or a local `main`.
    pub default_branch: Option<String>,
    /// The default branch's tip, e.g. `origin/main`'s commit.
    pub default_tip: Option<String>,
    pub unpushed: HashSet<String>,
    pub unpulled: HashSet<String>,
}

impl Context {
    /// Whether local branch `name` is the default branch or tracks it.
    fn is_default(&self, name: &str) -> bool {
        self.default_branch
            .as_deref()
            .is_some_and(|d| d == name || d.split_once('/').is_some_and(|(_, n)| n == name))
    }
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

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Branch names that nodes point into.
    pub branches: Vec<String>,
    /// The most lanes any row used.
    pub width: usize,
    lanes: Vec<Option<Slot>>,
    /// Commits still to come that are on the default branch's first-parent
    /// line, or merged into it.
    trunk: HashSet<String>,
    merged: HashSet<String>,
    branch_ids: HashMap<String, usize>,
    /// Edges waiting for a parent not loaded yet, by the parent's id, so
    /// closing one looks only at the edges into that commit.
    open: HashMap<String, Vec<usize>>,
    /// Closed edges spanning more than `LONG` rows: the only ones besides
    /// the open ones that can cross a range from a child far before it.
    long: Vec<usize>,
}

/// Rows an edge may span and still be found by its child's row alone.
const LONG: usize = 32;

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

    /// The edges that can cross rows `lo..=hi`, in the order they were
    /// added: those starting near the range, found by binary search since
    /// edges are added in child-row order, plus the long and open ones
    /// reaching into it from further back.
    pub fn visible_edges(&self, lo: usize, hi: usize) -> impl Iterator<Item = &Edge> {
        let rows = self.nodes.len();
        let reaches = move |e: &Edge| e.parent.map_or(rows, |p| p.0) >= lo && e.child.0 <= hi;
        let near = self.edges.partition_point(|e| e.child.0 + LONG < lo);
        let end = self.edges.partition_point(|e| e.child.0 <= hi).max(near);
        let mut far: Vec<usize> = self
            .long
            .iter()
            .chain(self.open.values().flatten())
            .copied()
            .filter(|&i| i < near && reaches(&self.edges[i]))
            .collect();
        far.sort_unstable();
        far.into_iter()
            .map(|i| &self.edges[i])
            .chain(self.edges[near..end].iter().filter(move |e| reaches(e)))
    }

    /// The lowest lane no noodle runs in between rows `row - 1` and `row`,
    /// for a marker drawn in that gap.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn free_lane_above(&self, row: usize) -> usize {
        let Some(prev) = row.checked_sub(1) else {
            return 0;
        };
        let mut used = HashSet::new();
        for e in self.visible_edges(prev, row) {
            for seg in e.segments(self.nodes.len()) {
                let (Seg::Straight(a, b) | Seg::Curve(a, b)) = seg;
                if a.row <= prev as f32 && b.row >= row as f32 {
                    used.insert(a.lane as usize);
                    used.insert(b.lane as usize);
                }
            }
        }
        (0..).find(|l| !used.contains(l)).unwrap_or(0)
    }

    /// Edges still waiting for a parent below the last loaded row.
    #[cfg(test)]
    pub fn open_edges(&self) -> impl Iterator<Item = &Edge> {
        self.open.values().flatten().map(|&i| &self.edges[i])
    }

    fn add(&mut self, commit: &Commit, ctx: &Context) {
        let row = self.nodes.len();

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
        let mut open = self.open.remove(&commit.id).unwrap_or_default();
        for &i in &waiting {
            let slot = self.lanes[i].take().expect("waiting lane");
            for s in slot.sources {
                self.close(&mut open, s.child, i, (row, lane), s.first);
            }
        }
        if !open.is_empty() {
            self.open.insert(commit.id.clone(), open);
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
            self.open
                .entry(parent.clone())
                .or_default()
                .push(self.edges.len());
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

    /// Ends the edge from `child` through lane `via` at the parent, out of
    /// `open`, the edges still waiting for that parent.
    fn close(
        &mut self,
        open: &mut Vec<usize>,
        child: (usize, usize),
        via: usize,
        at: (usize, usize),
        first: bool,
    ) {
        let found = open.iter().position(|&i| {
            let e = &self.edges[i];
            e.child == child && e.via == via && e.first == first
        });
        if let Some(n) = found {
            let i = open.swap_remove(n);
            self.edges[i].parent = Some(at);
            if at.0 > child.0 + LONG {
                self.long.push(i);
            }
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
        // A local branch also takes over from one carried down, so stacked
        // branches each keep their own colour, except from HEAD's, which
        // runs straight down its lane, and from its own remote. On the
        // default branch's line only that branch does, so a merged branch
        // never deleted doesn't rename the trunk below it.
        let refs = ctx.refs.at(id);
        let local = refs.iter().find(|r| matches!(r, Ref::Local(_)));
        let own = local
            .or_else(|| refs.iter().find(|r| matches!(r, Ref::Remote(_))))
            .map(|r| self.branch_id(r.name()));
        let branch = match (inherited, local) {
            (Some(b), Some(local)) if self.takes_over(b, local.name(), on_trunk, ctx) => own,
            _ => inherited.or(own),
        };

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

    /// Whether a local branch `name` on a commit starts a new branch below
    /// the one carried down, `carried`.
    fn takes_over(&self, carried: usize, name: &str, on_trunk: bool, ctx: &Context) -> bool {
        if on_trunk && !ctx.is_default(name) {
            return false;
        }
        let carried = self.branches[carried].as_str();
        let own_remote = carried.split_once('/').is_some_and(|(_, n)| n == name)
            && ctx.refs.tip(&Ref::Remote(carried.to_string())).is_some();
        carried != name && Some(carried) != ctx.refs.head_branch.as_deref() && !own_remote
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
    // Long handles, so a bend leaves and joins its lanes gently instead of
    // kinking where the straight run ends.
    const K: f32 = 0.75;
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
            ctx.refs.insert(id, r.clone());
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
            f_down.segments(4).collect::<Vec<_>>(),
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
        assert_eq!(
            open.segments(2).collect::<Vec<_>>(),
            vec![Seg::Straight(pos(1, 0), pos(2, 0))]
        );
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
    fn stacked_branches_keep_their_own_names() {
        //   b2     topic2, on top of topic1
        //   b1     topic1, on top of main (HEAD)
        //   a1
        //   m      main
        //   r
        let commits = [
            c("b2", &["b1"]),
            c("b1", &["a1"]),
            c("a1", &["m"]),
            c("m", &["r"]),
            c("r", &[]),
        ];
        let mut stacked = ctx(
            "m",
            &[
                ("b2", Ref::Local("topic2".into())),
                ("b1", Ref::Local("topic1".into())),
                ("m", Ref::Local("main".into())),
            ],
        );
        stacked.refs.head_branch = Some("main".into());
        let l = layout(&commits, &stacked);
        let branch = |row: usize| l.nodes[row].branch.map(|b| l.branches[b].as_str());
        assert_eq!(branch(0), Some("topic2"));
        assert_eq!(branch(1), Some("topic1"));
        assert_eq!(branch(2), Some("topic1"));
        assert_eq!(branch(3), Some("main"));
        assert_eq!(branch(4), Some("main"));

        // HEAD's branch runs on down past other branches' tips.
        let mut on_top = stacked.clone();
        on_top.refs.head = Some("b2".into());
        on_top.refs.head_branch = Some("topic2".into());
        let l = layout(&commits, &on_top);
        let branch = |row: usize| l.nodes[row].branch.map(|b| l.branches[b].as_str());
        assert_eq!(branch(1), Some("topic2"));
        assert_eq!(branch(3), Some("topic2"));

        // A local branch behind its own remote stays the remote's colour.
        let behind = ctx(
            "m",
            &[
                ("x", Ref::Remote("origin/topic1".into())),
                ("b1", Ref::Local("topic1".into())),
            ],
        );
        let commits = [c("x", &["b1"]), c("b1", &["m"]), c("m", &[])];
        let l = layout(&commits, &behind);
        let branch = |row: usize| l.nodes[row].branch.map(|b| l.branches[b].as_str());
        assert_eq!(branch(1), Some("origin/topic1"));
    }

    #[test]
    fn stale_branches_dont_rename_the_trunk() {
        //   m2     main, origin/main
        //   | f1   f (HEAD)
        //   m1 |   fix-123, merged long ago and never deleted
        //   |/
        //   m0
        let commits = [
            c("m2", &["m1"]),
            c("f1", &["m0"]),
            c("m1", &["m0"]),
            c("m0", &[]),
        ];
        let mut ctx = ctx(
            "f1",
            &[
                ("m2", Ref::Local("main".into())),
                ("m2", Ref::Remote("origin/main".into())),
                ("f1", Ref::Local("f".into())),
                ("m1", Ref::Local("fix-123".into())),
            ],
        );
        ctx.refs.head_branch = Some("f".into());
        ctx.default_branch = Some("origin/main".into());
        ctx.default_tip = Some("m2".into());
        let l = layout(&commits, &ctx);
        let branch = |row: usize| l.nodes[row].branch.map(|b| l.branches[b].as_str());
        assert_eq!(branch(0), Some("main"));
        assert_eq!(branch(2), Some("main"));

        // Off the trunk a stacked branch still takes over.
        ctx.default_tip = None;
        let l = layout(&commits, &ctx);
        let branch = |row: usize| l.nodes[row].branch.map(|b| l.branches[b].as_str());
        assert_eq!(branch(2), Some("fix-123"));
    }

    #[test]
    fn free_lane_above_skips_noodles_into_head() {
        //   m      merges h in as its second parent
        //   | x    unpulled, on top of h
        //   h |    HEAD
        //   |/
        //   r
        let commits = [
            c("m", &["r", "h"]),
            c("x", &["h"]),
            c("h", &["r"]),
            c("r", &[]),
        ];
        let l = layout(&commits, &ctx("h", &[]));
        assert_eq!(l.nodes[2].lane, 0);
        let free = l.free_lane_above(2);
        // m's noodle into h runs along lane 0, x bends in from its own.
        assert_ne!(free, 0);
        assert_ne!(free, l.nodes[1].lane);
        assert_eq!(l.free_lane_above(0), 0);
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
            e.segments(10).collect::<Vec<_>>(),
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
        assert_eq!(
            hop.segments(10).collect::<Vec<_>>(),
            vec![Seg::Curve(pos(3, 1), pos(4, 0))]
        );
    }

    #[test]
    fn noodle_controls_follow_the_flow() {
        // Strip: rows run right to left.
        let [_, c1, c2, _] = noodle((100.0, 0.0), (40.0, 10.0), (-1.0, 0.0));
        assert_eq!(c1, (55.0, 0.0));
        assert_eq!(c2, (85.0, 10.0));
        // Column: rows run down.
        let [_, c1, c2, _] = noodle((0.0, 0.0), (10.0, 20.0), (0.0, 1.0));
        assert_eq!(c1, (0.0, 15.0));
        assert_eq!(c2, (10.0, 5.0));
    }

    #[test]
    fn visible_edges_are_the_ones_crossing_the_range() {
        // A trunk with side branches of every length, the longest far over
        // LONG, merged back; the last page leaves edges open.
        let mut commits = Vec::new();
        let n: usize = 400;
        for i in (0..n).rev() {
            let id = format!("t{i}");
            let mut parents = Vec::new();
            if i > 0 {
                parents.push(format!("t{}", i - 1));
            }
            if i % 7 == 0 && i > 0 {
                parents.push(format!("b{i}"));
            }
            commits.push(c(
                &id,
                &parents.iter().map(String::as_str).collect::<Vec<_>>(),
            ));
            if i % 7 == 0 && i > 0 {
                let base = i.saturating_sub(1 + (i * 13) % 90);
                commits.push(c(&format!("b{i}"), &[&format!("t{base}")]));
            }
        }
        let ctx = ctx("t399", &[]);
        let mut l = Layout::new(&ctx);
        for page in commits[..commits.len() - 40].chunks(100) {
            l.push(page, &ctx);
        }
        assert!(l.open_edges().count() > 0 && !l.long.is_empty());
        let rows = l.nodes.len();
        for (lo, hi) in [
            (0, 10),
            (50, 70),
            (100, 101),
            (200, 260),
            (rows - 5, rows),
            (rows, rows + 9),
        ] {
            let want: Vec<&Edge> = l
                .edges
                .iter()
                .filter(|e| e.parent.map_or(rows, |p| p.0) >= lo && e.child.0 <= hi)
                .collect();
            let got: Vec<&Edge> = l.visible_edges(lo, hi).collect();
            assert_eq!(got, want, "rows {lo}..={hi}");
        }
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
