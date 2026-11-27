package gitstat.core

import kotlinx.serialization.Serializable

/** A commit as the tree needs it. [time] is in seconds since 1970. */
@Serializable
data class Commit(
    val id: String,
    val parents: List<String>,
    val time: Long,
    val subject: String,
    val author: String = "",
)

/** What a dot's colour says, as on the PC (src/graph.rs `Class` and the PR states). */
enum class Ink {
    /** On the default branch's first-parent line. */
    TRUNK,

    /** Merged into the default branch. */
    MERGED,

    /** On a branch with an open pull request. */
    PR_OPEN,

    /** On a branch with a draft pull request. */
    PR_DRAFT,

    /** On a branch of its own, coloured by a hue hashed from its name ([Tree.hue]). */
    BRANCH,
}

data class Node(val lane: Int, val ink: Ink, val branch: Int?, val merge: Boolean)

/**
 * A noodle from a child down to one of its parents: it leaves the child, bends into lane
 * [via], runs along it and bends into the parent. [parentRow] is null while the parent isn't
 * loaded: the noodle runs on past the last row.
 */
data class Edge(
    val childRow: Int,
    val childLane: Int,
    val via: Int,
    val parentRow: Int?,
    val parentLane: Int?,
    val first: Boolean,
)

/** A piece of a noodle, in rows and lanes. [curve]: an S-bend between lanes. */
data class Seg(val row1: Float, val lane1: Float, val row2: Float, val lane2: Float, val curve: Boolean)

/**
 * A repo's history laid out as on the PC (a port of src/graph.rs): a lane per line of
 * development, a row per commit, newest first. Lane 0 is the default branch, so the trunk is
 * the straight line. A commit takes the lowest lane already waiting for it, else a free one;
 * its first parent continues in its lane, other parents join a lane waiting for them or open
 * one; two lanes waiting for one commit meet on its dot.
 */
class Tree private constructor(
    val nodes: List<Node>,
    val edges: List<Edge>,
    val branches: List<String>,
    /** The most lanes any row used. */
    val width: Int,
) {
    val rows: Int get() = nodes.size

    fun inkOf(row: Int): Ink = nodes[row].ink

    /** The hue index of [row]'s branch, `0 until n`, stable across runs (FNV-1a, as on the PC). */
    fun hueOf(row: Int, n: Int): Int {
        val node = nodes[row]
        return node.branch?.let { hue(branches[it], n) } ?: (node.lane % n)
    }

    companion object {
        fun hue(name: String, n: Int): Int {
            var h = 0x811c9dc5.toInt()
            for (b in name.toByteArray()) {
                h = h xor (b.toInt() and 0xff)
                h *= 0x01000193
            }
            return ((h.toLong() and 0xffffffffL) % n.coerceAtLeast(1)).toInt()
        }

        /** The noodle as straight runs and bends; one still waiting for its parent ends at [lastRow]. */
        fun segments(e: Edge, lastRow: Int): List<Seg> {
            val r1 = e.childRow
            val l1 = e.childLane
            val r2 = e.parentRow ?: maxOf(lastRow, r1 + 1)
            val l2 = e.parentLane ?: e.via
            val v = e.via
            fun seg(a: Int, la: Int, b: Int, lb: Int, curve: Boolean) =
                Seg(a.toFloat(), la.toFloat(), b.toFloat(), lb.toFloat(), curve)
            if (r2 <= r1 + 1) return listOf(seg(r1, l1, r2, l2, l1 != l2))
            val out = mutableListOf<Seg>()
            var atRow = r1
            var atLane = l1
            if (l1 != v) {
                out += seg(atRow, atLane, r1 + 1, v, true)
                atRow = r1 + 1
                atLane = v
            }
            val bend = if (l2 != v) r2 - 1 else r2
            if (atRow < bend) {
                out += seg(atRow, atLane, bend, v, false)
                atRow = bend
            }
            if (l2 != v) out += seg(atRow, atLane, r2, l2, true)
            return out
        }

        /**
         * Lays out [commits], which must be in [order]. [tips] names the branches at each commit;
         * [prs] gives a branch's PR, open or draft, by branch name.
         */
        fun layout(
            commits: List<Commit>,
            tips: Map<String, List<String>>,
            defaultBranch: String,
            defaultTip: String?,
            prs: Map<String, Pr> = emptyMap(),
        ): Tree = Builder(tips, defaultBranch, prs).apply {
            if (defaultTip != null) {
                lanes += Slot(defaultTip)
                trunk += defaultTip
            }
            commits.forEach { add(it) }
        }.build()

        /**
         * Newest first, but every commit after all its loaded children (GitHub's dates can run
         * backwards across a rebase). A port of src/github.rs `date_order`.
         */
        fun order(commits: List<Commit>): List<Commit> {
            val index = commits.withIndex().associate { (i, c) -> c.id to i }
            val children = IntArray(commits.size)
            for (c in commits) for (p in c.parents) index[p]?.let { children[it]++ }
            val ready = java.util.PriorityQueue<Int>(
                compareByDescending<Int> { commits[it].time }.thenBy { it },
            )
            children.withIndex().filter { it.value == 0 }.forEach { ready += it.index }
            val out = ArrayList<Commit>(commits.size)
            while (ready.isNotEmpty()) {
                val i = ready.poll()
                out += commits[i]
                for (p in commits[i].parents) {
                    val j = index[p] ?: continue
                    if (--children[j] == 0) ready += j
                }
            }
            return out
        }
    }

    private class Slot(val target: String, val sources: MutableList<Source> = mutableListOf())

    private class Source(val childRow: Int, val childLane: Int, val first: Boolean, val branch: Int?)

    private class Builder(
        val tips: Map<String, List<String>>,
        val defaultBranch: String,
        val prs: Map<String, Pr>,
    ) {
        val nodes = mutableListOf<Node>()
        val edges = mutableListOf<Edge>()
        val branches = mutableListOf<String>()
        val branchIds = mutableMapOf<String, Int>()
        val lanes = mutableListOf<Slot?>()
        val trunk = mutableSetOf<String>()
        val merged = mutableSetOf<String>()
        /** Edges waiting for a parent not loaded yet, by the parent's id. */
        val open = mutableMapOf<String, MutableList<Int>>()
        var width = 0

        fun build() = Tree(nodes, edges, branches, maxOf(width, 1))

        fun add(commit: Commit) {
            val row = nodes.size
            val waiting = lanes.indices.filter { lanes[it]?.target == commit.id }
            val lane = waiting.firstOrNull() ?: freeLane()
            val inherited = lanes[lane]?.sources?.firstOrNull { it.first }?.branch
            val pending = open.remove(commit.id) ?: mutableListOf()
            for (i in waiting) {
                val slot = lanes[i]!!
                lanes[i] = null
                for (s in slot.sources) close(pending, s, i, row, lane)
            }
            if (pending.isNotEmpty()) open[commit.id] = pending

            val onTrunk = trunk.remove(commit.id)
            val isMerged = onTrunk || merged.remove(commit.id)
            if (onTrunk) commit.parents.firstOrNull()?.let { trunk += it }
            if (isMerged) merged += commit.parents

            // The first-parent child's branch carries down; a tip nothing carries to takes the
            // branch pointing at it. A branch of its own takes over below another one (stacked
            // branches keep their colours), but on the trunk only the default branch does.
            val here = tips[commit.id].orEmpty()
            val local = if (defaultBranch in here) defaultBranch else here.firstOrNull()
            val branch = when {
                inherited == null -> local?.let { id(it) }
                local != null && branches[inherited] != local && (!onTrunk || local == defaultBranch) -> id(local)
                else -> inherited
            }
            val pr = branch?.let { prs[branches[it]] }
            val ink = when {
                onTrunk -> Ink.TRUNK
                isMerged -> Ink.MERGED
                pr != null -> if (pr.draft) Ink.PR_DRAFT else Ink.PR_OPEN
                else -> Ink.BRANCH
            }
            nodes += Node(lane, ink, branch, commit.parents.size > 1)

            commit.parents.forEachIndexed { i, parent ->
                val first = i == 0
                val via = if (first) {
                    lanes[lane] = Slot(parent)
                    lane
                } else {
                    lanes.indexOfFirst { it?.target == parent }.takeIf { it >= 0 }
                        ?: freeLane().also { lanes[it] = Slot(parent) }
                }
                lanes[via]!!.sources += Source(row, lane, first, branch)
                open.getOrPut(parent) { mutableListOf() } += edges.size
                edges += Edge(row, lane, via, null, null, first)
            }
            while (lanes.isNotEmpty() && lanes.last() == null) lanes.removeAt(lanes.size - 1)
            width = maxOf(width, lanes.size, lane + 1)
        }

        fun close(pending: MutableList<Int>, s: Source, via: Int, row: Int, lane: Int) {
            val n = pending.indexOfFirst {
                val e = edges[it]
                e.childRow == s.childRow && e.childLane == s.childLane && e.via == via && e.first == s.first
            }
            if (n < 0) return
            val i = pending.removeAt(n)
            edges[i] = edges[i].copy(parentRow = row, parentLane = lane)
        }

        fun freeLane(): Int {
            val i = lanes.indexOfFirst { it == null }
            if (i >= 0) return i
            lanes += null
            return lanes.size - 1
        }

        fun id(name: String): Int = branchIds.getOrPut(name) { branches += name; branches.size - 1 }
    }
}

/** A repo's tree, from what the snapshot holds. */
fun Repo.tree(): Tree {
    val tips = mutableMapOf<String, MutableList<String>>()
    for (b in branches) if (b.tip.isNotEmpty()) tips.getOrPut(b.tip) { mutableListOf() } += b.name
    if (defaultTip.isNotEmpty()) tips.getOrPut(defaultTip) { mutableListOf() }.add(0, defaultBranch)
    val prs = openPrs.associateBy { it.headBranch }
    return Tree.layout(commits, tips, defaultBranch, defaultTip.ifEmpty { null }, prs)
}
