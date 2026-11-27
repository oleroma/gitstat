package gitstat.core

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class GitHubTest {
    private val reposPage = """
        {"data":{"viewer":{"login":"olex","repositories":{
          "pageInfo":{"hasNextPage":false,"endCursor":null},
          "nodes":[
            {"nameWithOwner":"olex/tidy","url":"https://github.com/olex/tidy","isPrivate":true,
             "isFork":false,"pushedAt":"2026-11-20T10:00:00Z","defaultBranchRef":{"name":"main"}},
            {"nameWithOwner":"olex/hanging","url":"https://github.com/olex/hanging","isPrivate":false,
             "isFork":false,"pushedAt":"2026-11-01T10:00:00Z","defaultBranchRef":{"name":"main"}},
            {"nameWithOwner":"olex/empty","url":"https://github.com/olex/empty","isPrivate":false,
             "isFork":false,"pushedAt":"2026-11-02T10:00:00Z","defaultBranchRef":null}
          ]}}}}
    """.trimIndent()

    private val details = """
        {"data":{
          "r0":{"defaultBranchRef":{"target":{"statusCheckRollup":{"state":"SUCCESS"}}},
            "refs":{"totalCount":3,"nodes":[
              {"name":"main","target":{"oid":"a1","committedDate":"2026-11-20T10:00:00Z"},
               "compare":{"aheadBy":0,"behindBy":0}},
              {"name":"old-feature","target":{"oid":"b1","committedDate":"2026-10-01T10:00:00Z"},
               "compare":{"aheadBy":12,"behindBy":0}},
              {"name":"squashed","target":{"oid":"c1","committedDate":"2026-10-02T10:00:00Z"},
               "compare":{"aheadBy":3,"behindBy":4}}
            ]},
            "open":{"nodes":[]},
            "merged":{"nodes":[{"headRefName":"squashed","headRefOid":"c1","isCrossRepository":false}]},
            "icon":{"text":"<svg/>","byteSize":6,"isBinary":false}},
          "r1":{"defaultBranchRef":{"target":{"statusCheckRollup":null}},
            "refs":{"totalCount":3,"nodes":[
              {"name":"main","target":{"oid":"d1"},"compare":{"aheadBy":0,"behindBy":0}},
              {"name":"wip/phone app","target":{"oid":"e1","committedDate":"2026-11-01T10:00:00Z"},
               "compare":{"aheadBy":1,"behindBy":5}},
              {"name":"reused","target":{"oid":"f2"},"compare":{"aheadBy":0,"behindBy":2}}
            ]},
            "open":{"nodes":[
              {"number":7,"title":"Phone app","url":"https://github.com/olex/hanging/pull/7",
               "isDraft":true,"headRefName":"wip/phone app","isCrossRepository":false},
              {"number":8,"title":"From a fork","url":"https://github.com/olex/hanging/pull/8",
               "isDraft":false,"headRefName":"reused","isCrossRepository":true}
            ]},
            "merged":{"nodes":[{"headRefName":"reused","headRefOid":"f1","isCrossRepository":false}]}}
        }}
    """.trimIndent()

    private fun fake(vararg answers: String): Pair<GitHub, MutableList<String>> {
        val asked = mutableListOf<String>()
        val queue = ArrayDeque(answers.toList())
        return GitHub { q -> asked += q; queue.removeFirst() } to asked
    }

    @Test
    fun readsReposAndColoursThem() {
        val (gh, asked) = fake(reposPage, details)
        val snap = gh.read(now = 42)
        assertEquals("olex", snap.login)
        assertEquals(42, snap.readAt)
        // The empty repo has no default branch, so there's nothing to compare: it's left out.
        assertEquals(listOf("olex/tidy", "olex/hanging"), snap.repos.map { it.slug })
        assertEquals(2, asked.size)
        assertTrue(asked.all { it.startsWith("query{") }, "only read-only queries")

        val tidy = snap.repos[0]
        // A branch main is ahead of, and one whose tip a merged PR took in (squash), are merged.
        assertEquals(Health.GREEN, tidy.health)
        assertEquals(Checks.PASSING, tidy.checksState)
        assertEquals(listOf("old-feature", "squashed"), tidy.branches.map { it.name })
        assertEquals(12, tidy.branches[0].mainAhead)
        assertTrue(tidy.branches[1].mergedByPr)
        assertEquals("<svg/>", tidy.icon)

        val hanging = snap.repos[1]
        assertEquals(Health.RED, hanging.health)
        assertEquals(listOf("wip/phone app", "reused"), hanging.hanging.map { it.name })
        val wip = hanging.branches[0]
        assertEquals(5, wip.notInMain)
        assertEquals(1, wip.mainAhead)
        assertEquals(7, wip.pr?.number)
        assertTrue(wip.pr!!.draft)
        assertEquals("https://github.com/olex/hanging/tree/wip/phone%20app", wip.url)
        // A merged PR from another tip doesn't merge the branch's new commits, and fork PRs
        // aren't this repo's branches.
        assertFalse(hanging.branches[1].mergedByPr)
        assertEquals(null, hanging.branches[1].pr)
        assertEquals(listOf(7), hanging.openPrs.map { it.number })
        assertEquals(null, hanging.icon)
    }

    @Test
    fun comparesEachBranchWithItsRepoDefaultBranch() {
        val repo = Repo("o/n\"x", "u", false, false, "", "trunk\"")
        val q = GitHub.detailsQuery(listOf(repo))
        assertTrue("compare(headRef:\"trunk\\\"\")" in q, q)
        assertTrue("name:\"n\\\"x\"" in q, q)
        assertFalse("mutation" in q)
    }

    @Test
    fun aRepoGitHubCouldNotReadIsGrey() {
        val (gh, _) = fake(reposPage, """{"data":{"r0":null,"r1":null},"errors":[{"message":"timeout"}]}""")
        val snap = gh.read()
        assertTrue(snap.repos.all { it.health == Health.UNKNOWN })
    }

    @Test
    fun aBatchThatFailsMarksOnlyItsRepos() {
        val (gh, _) = fake(reposPage, """{"errors":[{"message":"Something went wrong"}]}""")
        val snap = gh.read()
        assertEquals(listOf("Something went wrong", "Something went wrong"), snap.repos.map { it.problem })
    }

    @Test
    fun aRefusedTokenStopsTheRead() {
        val gh = GitHub { throw GitHubException("bad token", auth = true) }
        assertFailsWith<GitHubException> { gh.read() }
    }

    @Test
    fun followsRepoPages() {
        val page1 = reposPage.replace(
            "\"hasNextPage\":false,\"endCursor\":null",
            "\"hasNextPage\":true,\"endCursor\":\"abc\"",
        )
        val page2 = """{"data":{"viewer":{"login":"olex","repositories":{
            "pageInfo":{"hasNextPage":false},"nodes":[]}}}}"""
        val (gh, asked) = fake(page1, page2, details)
        gh.read()
        assertTrue("after:\"abc\"" in asked[1], asked[1])
    }

    @Test
    fun sortsRedFirstAndFindsNewlyRed() {
        val (gh, _) = fake(reposPage, details)
        val now = gh.read()
        assertEquals(listOf("olex/hanging", "olex/tidy"), now.sorted().map { it.slug })
        assertEquals(1, now.redCount)
        assertEquals(emptyList(), now.newlyRed(null))
        val before = now.copy(repos = now.repos.map { it.copy(branches = emptyList()) })
        assertEquals(listOf("olex/hanging"), now.newlyRed(before).map { it.slug })
        assertEquals(emptyList(), now.newlyRed(now))
    }
}

class SnapshotTest {
    @Test
    fun roundTripsThroughJson() {
        val s = Snapshot(
            "olex", 1,
            listOf(Repo("o/n", "u", true, false, "p", "main", branches = listOf(Branch("b", "u", 1, 2)))),
        )
        assertEquals(s, Snapshot.fromJson(s.toJson()))
        assertEquals(null, Snapshot.fromJson("not json"))
    }
}

class QueryTest {
    @Test
    fun queriesAreBalanced() {
        val q = GitHub.detailsQuery(listOf(Repo("o/a", "u", false, false, "", "main"), Repo("o/b", "u", false, false, "", "dev")))
        var depth = 0
        for (ch in q) {
            if (ch == '{') depth++
            if (ch == '}') depth--
            assertTrue(depth >= 0, q)
        }
        assertEquals(0, depth, q)
        assertTrue("history(first:${GitHub.MAIN_DEPTH})" in q)
    }
}

class CommitDetailTest {
    @Test
    fun readsACommitsMessageAndSize() {
        var asked = ""
        val gh = GitHub { q ->
            asked = q
            """{"data":{"repository":{"object":{"message":"Subject\n\nBody line\n","additions":3,"deletions":1,"changedFilesIfAvailable":2}}}}"""
        }
        val d = gh.commit(Repo("o/n", "u", false, false, "", "main"), "abc")
        assertEquals("Subject\n\nBody line", d.message)
        assertEquals(3, d.additions)
        assertEquals(2, d.files)
        assertTrue("object(oid:\"abc\")" in asked, asked)
    }
}
