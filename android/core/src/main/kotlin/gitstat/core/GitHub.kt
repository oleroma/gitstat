package gitstat.core

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.put
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

/**
 * Reads the account's repos from GitHub's GraphQL API, strictly read-only like gitstat on the
 * PC: every request is a GraphQL `query`, never a mutation, sent as a POST to /graphql because
 * that is how GitHub's GraphQL API takes every request.
 *
 * One query lists the repos (100 a page, the same repos gitstat's GitHub list shows: the
 * account's own and its organisations', not archived), then one query per [BATCH] repos reads
 * every branch compared with the default branch, plus the open and merged pull requests.
 */
class GitHub(private val post: (query: String) -> String) {

    /** Reads everything; throws [GitHubException] when GitHub can't be read at all. */
    fun read(now: Long = System.currentTimeMillis()): Snapshot {
        val (login, listed) = listRepos()
        val repos = listed.chunked(BATCH).flatMap { batch ->
            try {
                details(batch)
            } catch (e: GitHubException) {
                if (e.fatal) throw e
                batch.map { it.copy(problem = e.message) }
            }
        }
        return Snapshot(login, now, repos)
    }

    private fun listRepos(): Pair<String, List<Repo>> {
        val repos = mutableListOf<Repo>()
        var login = ""
        var after: String? = null
        for (page in 0 until MAX_REPO_PAGES) {
            val data = ask(reposQuery(after))
            val viewer = data.obj("viewer")
            login = viewer.str("login") ?: login
            val list = viewer.obj("repositories")
            for (n in list.arr("nodes")) {
                val o = n as? JsonObject ?: continue
                val slug = o.str("nameWithOwner") ?: continue
                val default = o.obj("defaultBranchRef").str("name") ?: continue // empty repo
                if (!SLUG.matches(slug)) continue
                repos += Repo(
                    slug = slug,
                    url = o.str("url") ?: "https://github.com/$slug",
                    private = o.bool("isPrivate"),
                    fork = o.bool("isFork"),
                    pushedAt = o.str("pushedAt") ?: "",
                    defaultBranch = default,
                )
            }
            val info = list.obj("pageInfo")
            if (!info.bool("hasNextPage")) break
            after = info.str("endCursor") ?: break
        }
        return login to repos
    }

    private fun details(batch: List<Repo>): List<Repo> {
        val data = ask(detailsQuery(batch))
        return batch.mapIndexed { i, repo ->
            val o = data["r$i"] as? JsonObject
                ?: return@mapIndexed repo.copy(problem = "GitHub returned nothing for this repo")
            parseDetails(repo, o)
        }
    }

    /** Runs one query and returns its `data`; GraphQL errors with no data throw. */
    private fun ask(query: String): JsonObject {
        val text = post(query)
        val root = try {
            JSON.parseToJsonElement(text) as? JsonObject
        } catch (e: Exception) {
            null
        } ?: throw GitHubException("GitHub sent something that isn't JSON")
        val data = root["data"] as? JsonObject
        val errors = root.arr("errors").mapNotNull { (it as? JsonObject)?.str("message") }
        if (data == null) {
            throw GitHubException(errors.firstOrNull() ?: root.str("message") ?: "GitHub sent no data")
        }
        return data
    }

    companion object {
        /** Repos per details query; each repo reads up to [MAX_BRANCHES] compared branches. */
        const val BATCH = 5
        const val MAX_BRANCHES = 30
        /** Commits read from the default branch, and from each other branch, for the trees. */
        const val MAIN_DEPTH = 60
        const val BRANCH_DEPTH = 25
        const val MAX_OPEN = 30
        const val MAX_MERGED = 50
        const val MAX_REPO_PAGES = 10
        /** Larger repo icons are left out; the PC refuses ones over 1 MB, but these ride in every read. */
        const val MAX_ICON_BYTES = 64 * 1024
        private val SLUG = Regex("^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
        private val JSON = Json { ignoreUnknownKeys = true }

        fun reposQuery(after: String?): String {
            val cursor = if (after == null) "" else ",after:${quote(after)}"
            return "query{viewer{login repositories(first:100$cursor," +
                "ownerAffiliations:[OWNER,ORGANIZATION_MEMBER],isArchived:false," +
                "orderBy:{field:PUSHED_AT,direction:DESC})" +
                "{pageInfo{hasNextPage endCursor} " +
                "nodes{nameWithOwner url isPrivate isFork pushedAt defaultBranchRef{name}}}}}"
        }

        /**
         * One query for [batch]. Each branch is compared with the default branch through
         * `Ref.compare(headRef:)`, whose base is the branch and head the default branch: so
         * `behindBy` counts the branch's commits the default branch doesn't have, and `aheadBy`
         * the default branch's commits the branch doesn't have.
         */
        fun detailsQuery(batch: List<Repo>): String = buildString {
            append("query{")
            batch.forEachIndexed { i, r ->
                append("r$i:repository(owner:${quote(r.owner)},name:${quote(r.name)}){")
                append("defaultBranchRef{target{oid ...on Commit{statusCheckRollup{state} ")
                append("history(first:$MAIN_DEPTH){nodes{$COMMIT}}}}} ")
                append("refs(refPrefix:\"refs/heads/\",first:$MAX_BRANCHES,")
                append("orderBy:{field:TAG_COMMIT_DATE,direction:DESC}){totalCount ")
                append("nodes{name target{oid ...on Commit{committedDate ")
                append("history(first:$BRANCH_DEPTH){nodes{$COMMIT}}}} ")
                append("compare(headRef:${quote(r.defaultBranch)}){aheadBy behindBy}}} ")
                append("open:pullRequests(states:OPEN,first:$MAX_OPEN,")
                append("orderBy:{field:UPDATED_AT,direction:DESC})")
                append("{nodes{number title url isDraft headRefName isCrossRepository}} ")
                append("merged:pullRequests(states:MERGED,first:$MAX_MERGED,")
                append("orderBy:{field:UPDATED_AT,direction:DESC})")
                append("{nodes{headRefName headRefOid isCrossRepository}} ")
                append("icon:object(expression:\"HEAD:.gitstat/icon.svg\"){...on Blob{text byteSize isBinary}}")
                append("} ")
            }
            append("}")
        }

        private const val COMMIT = "oid committedDate messageHeadline author{name} parents(first:3){nodes{oid}}"

        /** A GraphQL string literal: JSON's escaping is GraphQL's. */
        fun quote(s: String): String = JsonPrimitive(s).toString()

        fun parseDetails(repo: Repo, o: JsonObject): Repo {
            val checks = when (o.obj("defaultBranchRef").obj("target").obj("statusCheckRollup").str("state")) {
                "SUCCESS" -> Checks.PASSING
                "FAILURE", "ERROR" -> Checks.FAILING
                "PENDING", "EXPECTED" -> Checks.PENDING
                else -> Checks.NONE
            }
            val open = o.obj("open").arr("nodes").mapNotNull { it as? JsonObject }
                .filter { !it.bool("isCrossRepository") }
                .mapNotNull { p ->
                    Pr(
                        number = p.int("number") ?: return@mapNotNull null,
                        title = p.str("title") ?: "",
                        url = p.str("url") ?: return@mapNotNull null,
                        draft = p.bool("isDraft"),
                        headBranch = p.str("headRefName") ?: "",
                    )
                }
            val mergedTips = o.obj("merged").arr("nodes").mapNotNull { it as? JsonObject }
                .filter { !it.bool("isCrossRepository") }
                .mapNotNull { p -> p.str("headRefName")?.let { it to (p.str("headRefOid") ?: "") } }
                .toSet()
            val refs = o.obj("refs")
            val branches = refs.arr("nodes").mapNotNull { it as? JsonObject }.mapNotNull { b ->
                val name = b.str("name") ?: return@mapNotNull null
                if (name == repo.defaultBranch) return@mapNotNull null
                val cmp = b.obj("compare")
                val target = b.obj("target")
                val oid = target.str("oid") ?: ""
                Branch(
                    name = name,
                    url = "${repo.url}/tree/${name.split('/').joinToString("/") { encode(it) }}",
                    notInMain = cmp.int("behindBy") ?: 0,
                    mainAhead = cmp.int("aheadBy") ?: 0,
                    committedAt = target.str("committedDate") ?: "",
                    pr = open.firstOrNull { it.headBranch == name },
                    mergedByPr = oid.isNotEmpty() && (name to oid) in mergedTips,
                    tip = oid,
                )
            }
            val seen = HashSet<String>()
            val commits = mutableListOf<Commit>()
            val histories = listOf(o.obj("defaultBranchRef").obj("target")) +
                refs.arr("nodes").mapNotNull { (it as? JsonObject)?.obj("target") }
            for (t in histories) {
                for (c in t.obj("history").arr("nodes")) {
                    val n = c as? JsonObject ?: continue
                    val id = n.str("oid") ?: continue
                    if (!seen.add(id)) continue
                    commits += Commit(
                        id = id,
                        parents = n.obj("parents").arr("nodes").mapNotNull { (it as? JsonObject)?.str("oid") },
                        time = n.str("committedDate")?.let { seconds(it) } ?: 0,
                        subject = (n.str("messageHeadline") ?: "").take(120),
                        author = n.obj("author").str("name") ?: "",
                    )
                }
            }
            val iconBlob = o.obj("icon")
            val icon = iconBlob.str("text")?.takeIf {
                !iconBlob.bool("isBinary") && (iconBlob.int("byteSize") ?: 0) <= MAX_ICON_BYTES
            }
            return repo.copy(
                icon = icon,
                defaultTip = o.obj("defaultBranchRef").obj("target").str("oid") ?: "",
                commits = Tree.order(commits),
                checks = checks.name,
                branches = branches,
                branchCount = refs.int("totalCount") ?: (branches.size + 1),
                openPrs = open,
                problem = null,
            )
        }

        private fun seconds(iso: String): Long? =
            runCatching { java.time.Instant.parse(iso).epochSecond }.getOrNull()

        private fun encode(s: String) = java.net.URLEncoder.encode(s, "UTF-8").replace("+", "%20")

        /** The real transport: an HTTPS POST to GitHub's GraphQL endpoint with [token]. */
        fun https(token: String): (String) -> String = { query ->
            val body = buildJsonObject { put("query", query) }.toString().toByteArray()
            val c = URL("https://api.github.com/graphql").openConnection() as HttpURLConnection
            try {
                c.requestMethod = "POST"
                c.connectTimeout = 20_000
                c.readTimeout = 60_000
                c.doOutput = true
                c.setRequestProperty("Authorization", "bearer $token")
                c.setRequestProperty("Content-Type", "application/json")
                c.setRequestProperty("User-Agent", "gitstat-phone")
                c.outputStream.use { it.write(body) }
                val code = c.responseCode
                val stream = if (code in 200..299) c.inputStream else c.errorStream
                val text = stream?.bufferedReader()?.use { it.readText() } ?: ""
                when (code) {
                    in 200..299 -> text
                    401 -> throw GitHubException("GitHub didn't accept the token. Sign in again.", auth = true)
                    403 -> throw GitHubException("GitHub refused: ${messageOf(text) ?: "rate limit or missing access"}", fatal = true)
                    else -> throw GitHubException("GitHub answered $code: ${messageOf(text) ?: "no details"}", fatal = true)
                }
            } catch (e: IOException) {
                throw GitHubException("Couldn't reach GitHub: ${e.message ?: e.javaClass.simpleName}", fatal = true)
            } finally {
                c.disconnect()
            }
        }

        private fun messageOf(text: String): String? = try {
            (JSON.parseToJsonElement(text) as? JsonObject)?.str("message")
        } catch (e: Exception) {
            null
        }
    }
}

/**
 * A read that failed. [auth]: the token was refused. [fatal]: GitHub couldn't be reached or
 * refused the whole request, so the read stops instead of marking repos one by one.
 */
class GitHubException(
    message: String,
    val auth: Boolean = false,
    val fatal: Boolean = auth,
) : Exception(message)

private val EMPTY = JsonObject(emptyMap())

private fun JsonObject.obj(key: String): JsonObject = this[key] as? JsonObject ?: EMPTY
private fun JsonObject.arr(key: String): JsonArray = this[key] as? JsonArray ?: JsonArray(emptyList())
private fun JsonObject.prim(key: String): JsonPrimitive? = (this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull }
private fun JsonObject.str(key: String): String? = prim(key)?.takeIf { it.isString }?.contentOrNull
private fun JsonObject.int(key: String): Int? = prim(key)?.intOrNull
private fun JsonObject.bool(key: String): Boolean = prim(key)?.booleanOrNull ?: false
