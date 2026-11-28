package gitstat.phone

import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import gitstat.core.Checks
import gitstat.core.Repo

/**
 * What a repo's info panel says, past the end of its tree: its state, its default branch,
 * branches, PRs and last push, and, unless [compact], which branches aren't merged and its
 * open PRs.
 */
@Composable
fun RepoFacts(repo: Repo, login: String, compact: Boolean, modifier: Modifier = Modifier) {
    val c = LocalColors.current
    val size = if (compact) 11.sp else 13.sp
    val hanging = repo.hanging
    Column(modifier, verticalArrangement = Arrangement.spacedBy(if (compact) 1.dp else 3.dp)) {
        if (!compact) {
            val tags = listOfNotNull(
                if (repo.private) "private" else "public",
                if (repo.fork) "fork" else null,
            )
            Text(
                (if (repo.owner != login) repo.slug else repo.name) + " · " + tags.joinToString(" · "),
                color = c.text,
                fontWeight = FontWeight.SemiBold,
                fontSize = 15.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        val state = listOfNotNull(
            when {
                repo.problem != null -> "couldn't read"
                hanging.isNotEmpty() -> "${branchCount(hanging.size)} not merged"
                else -> "all merged into ${repo.defaultBranch}"
            },
            when (repo.checksState) {
                Checks.PASSING -> "checks ✓"
                Checks.FAILING -> "checks ✗"
                Checks.PENDING -> "checks running"
                Checks.NONE -> null
            },
        )
        Text(
            state.joinToString(" · "),
            color = if (hanging.isNotEmpty() || repo.checksState == Checks.FAILING) c.red else c.green,
            fontSize = size,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        val others = (repo.branchCount - 1).coerceAtLeast(repo.branches.size)
        val counts = listOfNotNull(
            repo.defaultBranch,
            if (others == 1) "1 other branch" else "$others other branches",
            when (repo.openPrs.size) { 0 -> "no open PRs"; 1 -> "1 open PR"; else -> "${repo.openPrs.size} open PRs" },
            ago(repo.pushedAt).takeIf { it.isNotEmpty() }?.let { "pushed $it" },
        )
        Text(counts.joinToString(" · "), color = c.muted, fontSize = size, maxLines = if (compact) 1 else 2, overflow = TextOverflow.Ellipsis)
        if (compact) {
            if (hanging.isNotEmpty()) {
                Text(hanging.joinToString(", ") { it.name }, color = c.red, fontSize = size, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
        } else {
            repo.problem?.let { Text(it, color = c.amber, fontSize = size) }
            for (b in hanging.take(8)) {
                Text(
                    "• ${b.name}  +${b.notInMain}" + (b.pr?.let { "  #${it.number}" } ?: ""),
                    color = c.red,
                    fontSize = size,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            if (hanging.size > 8) Text("and ${hanging.size - 8} more", color = c.muted, fontSize = size)
            for (pr in repo.openPrs.take(5)) {
                Text(
                    "#${pr.number} ${pr.title}" + if (pr.draft) " (draft)" else "",
                    color = c.text,
                    fontSize = size,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

/**
 * Makes [scroll] settle at [a] or at [b] (a < b) when it stops anywhere between them: on the
 * side it was heading to, unless it barely moved off where it was. [onScroll] is called as a
 * scroll starts.
 */
@Composable
fun SnapBetween(scroll: ScrollState, a: Int, b: Int, onScroll: () -> Unit = {}) {
    var dir by remember(scroll) { mutableIntStateOf(0) }
    LaunchedEffect(scroll) {
        var prev = scroll.value
        snapshotFlow { scroll.value }.collect {
            if (it != prev) dir = if (it > prev) 1 else -1
            prev = it
        }
    }
    LaunchedEffect(scroll.isScrollInProgress, a, b) {
        if (scroll.isScrollInProgress) {
            onScroll()
            return@LaunchedEffect
        }
        val v = scroll.value
        if (b <= a || v <= a || v >= b) return@LaunchedEffect
        val nudge = (b - a) * 0.15f
        val to = when {
            dir > 0 -> if (v - a > nudge) b else a
            dir < 0 -> if (b - v > nudge) a else b
            else -> if (v - a > (b - a) / 2) b else a
        }
        scroll.animateScrollTo(to)
    }
}
