package gitstat.phone

import androidx.compose.animation.core.AnimationState
import androidx.compose.animation.core.DecayAnimationSpec
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.animateDecay
import androidx.compose.animation.core.calculateTargetValue
import androidx.compose.animation.core.spring
import androidx.compose.animation.rememberSplineBasedDecay
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.gestures.FlingBehavior
import androidx.compose.foundation.gestures.ScrollScope
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
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
 * A fling that settles a scroller with an info panel past its end at one of two points: [rest]
 * (the panel just out of sight) or [info] (the panel in full view). Between them it goes where
 * the swipe was heading, or to the nearer one when it barely moved. Past [rest], on the
 * history side ([historyBelow]: values below [rest]), it flings freely, but a fling from the
 * history doesn't run on into the panel: it stops at [rest].
 */
class PeekFling(
    private val rest: Int,
    private val info: Int,
    private val historyBelow: Boolean,
    private val decay: DecayAnimationSpec<Float>,
) : FlingBehavior {
    override suspend fun ScrollScope.performFling(initialVelocity: Float): Float {
        val lo = minOf(rest, info).toFloat()
        val hi = maxOf(rest, info).toFloat()
        // Where the scroller is as the fling starts (set by [peekFling]).
        val at = current
        val inHistory = if (historyBelow) at < lo - 0.5f else at > hi + 0.5f
        val landing = decay.calculateTargetValue(at, initialVelocity)
        val towardInfo = if (info > rest) initialVelocity else -initialVelocity
        val target: Float? = when {
            inHistory && (if (historyBelow) landing > rest else landing < rest) -> rest.toFloat()
            inHistory -> null
            towardInfo > FLICK -> info.toFloat()
            towardInfo < -FLICK -> rest.toFloat()
            else -> if (kotlin.math.abs(at - info) < kotlin.math.abs(at - rest)) info.toFloat() else rest.toFloat()
        }
        var velocityLeft = 0f
        if (target == null) {
            var last = at
            AnimationState(at, initialVelocity).animateDecay(decay) {
                val delta = value - last
                val used = scrollBy(delta)
                last = value
                velocityLeft = velocity
                if (kotlin.math.abs(delta - used) > 0.5f) cancelAnimation()
            }
            return velocityLeft
        }
        var last = at
        animate(at, target, initialVelocity, spring(dampingRatio = Spring.DampingRatioNoBouncy, stiffness = Spring.StiffnessMediumLow)) { value, _ ->
            scrollBy(value - last)
            last = value
        }
        return 0f
    }

    /** The scroller's position when a fling starts, set by [peekFling]. */
    var current = 0f

    private companion object {
        /** px/s past which a swipe counts as heading somewhere. */
        const val FLICK = 400f
    }
}

/** A [PeekFling] for [scroll], reading its position as each fling starts. */
@Composable
fun peekFling(scroll: ScrollState, rest: Int, info: Int, historyBelow: Boolean): FlingBehavior {
    val decay = rememberSplineBasedDecay<Float>()
    val fling = remember(rest, info, historyBelow, decay) { PeekFling(rest, info, historyBelow, decay) }
    return remember(fling, scroll) {
        object : FlingBehavior {
            override suspend fun ScrollScope.performFling(initialVelocity: Float): Float {
                fling.current = scroll.value.toFloat()
                return with(fling) { performFling(initialVelocity) }
            }
        }
    }
}
