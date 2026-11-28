package gitstat.core

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class TreeTest {
    private fun c(id: String, vararg parents: String, t: Long = 0) = Commit(id, parents.toList(), t, id)

    @Test
    fun trunkStaysInLaneZeroAndABranchForksBeside() {
        // main: m3 - m2 - m1 ; feature f2 - f1 forked from m1 and not merged.
        val commits = Tree.order(
            listOf(c("m3", "m2", t = 5), c("f2", "f1", t = 4), c("m2", "m1", t = 3), c("f1", "m1", t = 2), c("m1", t = 1)),
        )
        assertEquals(listOf("m3", "f2", "m2", "f1", "m1"), commits.map { it.id })
        val tree = Tree.layout(commits, mapOf("m3" to listOf("main"), "f2" to listOf("feature")), "main", "m3")
        assertEquals(listOf(0, 1, 0, 1, 0), tree.nodes.map { it.lane })
        assertEquals(listOf(Ink.TRUNK, Ink.BRANCH, Ink.TRUNK, Ink.BRANCH, Ink.TRUNK), tree.nodes.map { it.ink })
        assertEquals(2, tree.width)
        // f1's noodle bends from lane 1 into m1 in lane 0.
        val f1 = tree.edges.single { it.childRow == 3 }
        assertEquals(4, f1.parentRow)
        assertEquals(0, f1.parentLane)
        assertTrue(Tree.segments(f1, tree.rows).single().curve)
    }

    @Test
    fun mergedBranchesArePurpleAndPrBranchesTakeThePrColour() {
        // main: merge M of f1 into m1; open branch p1 off M with a draft PR.
        val commits = Tree.order(
            listOf(c("p1", "M", t = 4), c("M", "m1", "f1", t = 3), c("f1", "m0", t = 2), c("m1", "m0", t = 2), c("m0", t = 1)),
        )
        val tree = Tree.layout(
            commits,
            mapOf("M" to listOf("main"), "p1" to listOf("wip")),
            "main",
            "M",
            mapOf("wip" to Pr(1, "t", "u", draft = true, headBranch = "wip")),
        )
        val ink = commits.map { it.id }.zip(tree.nodes.map { it.ink }).toMap()
        assertEquals(Ink.PR_DRAFT, ink["p1"])
        assertEquals(Ink.TRUNK, ink["M"])
        assertEquals(Ink.MERGED, ink["f1"])
        assertEquals(Ink.TRUNK, ink["m0"])
        assertTrue(tree.nodes[commits.indexOfFirst { it.id == "M" }].merge)
    }

    @Test
    fun aParentNotLoadedLeavesTheNoodleRunningOn() {
        val tree = Tree.layout(listOf(c("a", "b")), emptyMap(), "main", "a")
        val e = tree.edges.single()
        assertEquals(null, e.parentRow)
        val seg = Tree.segments(e, tree.rows).single()
        assertEquals(1f, seg.row2)
    }

    @Test
    fun huesAreStable() {
        assertEquals(Tree.hue("feature", 6), Tree.hue("feature", 6))
        assertTrue((0 until 50).map { Tree.hue("b$it", 6) }.all { it in 0..5 })
    }
}
