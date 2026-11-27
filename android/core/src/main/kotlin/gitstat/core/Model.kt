package gitstat.core

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** What a repo's dot shows: the whole point of the phone view. */
enum class Health {
    /** Some branch has commits that aren't on the default branch yet: a hanging branch. */
    RED,

    /** Every branch is merged: the default branch has everything. */
    GREEN,

    /** Not read yet, or GitHub couldn't say. */
    UNKNOWN,
}

/** The default branch's combined checks, as gitstat's tray shows them. */
enum class Checks { PASSING, FAILING, PENDING, NONE }

@Serializable
data class Pr(
    val number: Int,
    val title: String,
    val url: String,
    val draft: Boolean,
    val headBranch: String,
)

@Serializable
data class Branch(
    val name: String,
    val url: String,
    /** Commits on this branch that the default branch doesn't have. */
    val notInMain: Int,
    /** Commits on the default branch that this branch doesn't have. */
    val mainAhead: Int,
    /** When its tip was committed (ISO 8601), or "" when GitHub didn't say. */
    val committedAt: String = "",
    /** Its open pull request, if it has one. */
    val pr: Pr? = null,
    /** Its tip is the head of a merged pull request (a squash or rebase merge). */
    val mergedByPr: Boolean = false,
    /** Its tip commit's id. */
    val tip: String = "",
) {
    /** A branch counts as hanging while it holds work the default branch doesn't have. */
    val hanging: Boolean get() = notInMain > 0 && !mergedByPr
}

@Serializable
data class Repo(
    /** `owner/name`. */
    val slug: String,
    val url: String,
    val private: Boolean,
    val fork: Boolean,
    val pushedAt: String,
    val defaultBranch: String,
    /** The default branch's checks, by name (see [Checks]). */
    val checks: String = Checks.NONE.name,
    /** Every branch but the default one, newest first. */
    val branches: List<Branch> = emptyList(),
    /** How many branches GitHub has, the default one included; more than were read when large. */
    val branchCount: Int = 0,
    val openPrs: List<Pr> = emptyList(),
    /** Set when this repo's details couldn't be read. */
    val problem: String? = null,
    /** The default branch's tip commit's id. */
    val defaultTip: String = "",
    /** Recent commits of every branch read, newest first with children before parents. */
    val commits: List<Commit> = emptyList(),
) {
    val name: String get() = slug.substringAfter('/')
    val owner: String get() = slug.substringBefore('/')
    val hanging: List<Branch> get() = branches.filter { it.hanging }
    val checksState: Checks get() = runCatching { Checks.valueOf(checks) }.getOrDefault(Checks.NONE)

    val health: Health
        get() = when {
            problem != null -> Health.UNKNOWN
            hanging.isNotEmpty() -> Health.RED
            else -> Health.GREEN
        }
}

/** One full read of the account's repos: what the app, the widget and the cache hold. */
@Serializable
data class Snapshot(
    val login: String,
    /** When it was read, in milliseconds since 1970. */
    val readAt: Long,
    val repos: List<Repo>,
) {
    /** Red repos first, then the rest; most recently pushed first within each. */
    fun sorted(): List<Repo> = repos.sortedWith(
        compareBy<Repo> { it.health.ordinal }.thenByDescending { it.pushedAt },
    )

    val redCount: Int get() = repos.count { it.health == Health.RED }

    /** The repos that are red now but weren't in [before] (a repo new since then counts when it starts red;
     * the very first read, with nothing [before], reports none). */
    fun newlyRed(before: Snapshot?): List<Repo> {
        if (before == null) return emptyList()
        val was = before.repos.associateBy { it.slug }
        return repos.filter { it.health == Health.RED && was[it.slug]?.health != Health.RED }
    }

    fun toJson(): String = JSON.encodeToString(serializer(), this)

    companion object {
        private val JSON = Json { ignoreUnknownKeys = true }

        /** The cached snapshot, or null when it's missing or from an older app that can't be read. */
        fun fromJson(text: String): Snapshot? = try {
            JSON.decodeFromString(serializer(), text)
        } catch (e: Exception) {
            null
        }
    }
}
