package gitstat.phone

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectDragGesturesAfterLongPress
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.LayoutCoordinates
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The commit under the finger while sweeping, and whether its card goes at the screen's top. */
data class Sweep(val picked: Picked, val top: Boolean)

/**
 * What the trees need from the screen: which commit is picked (repo slug and commit id), to
 * ring it, and where a sweep's commit goes.
 */
class Touch(val selected: Pair<String, String>?, val sweep: (Sweep?) -> Unit)

val LocalTouch = compositionLocalOf { Touch(null) {} }

/** Whether [repo]'s commit [id] is the picked one. */
@Composable
fun isPicked(slug: String, id: String): Boolean = LocalTouch.current.selected == (slug to id)

/**
 * A tree's touches. A tap picks the commit at the finger ([onCommit]). A hold starts a sweep:
 * as the finger moves, the commit nearest it is shown on a card ([LocalTouch]), with a tick
 * at each new one, and letting go opens that commit's full card. [rowAt] finds the commit's
 * row from a point in the tree, or null away from any.
 */
@Composable
fun Modifier.commitTouches(
    key: Any,
    rowAt: Density.(Offset, IntSize) -> Int?,
    pick: (Int) -> Picked,
    onCommit: (Picked) -> Unit,
): Modifier {
    val touch = LocalTouch.current
    val haptics = LocalHapticFeedback.current
    val screenH = with(LocalDensity.current) { LocalConfiguration.current.screenHeightDp.dp.toPx() }
    val sweep by rememberUpdatedState(touch.sweep)
    val commit by rememberUpdatedState(onCommit)
    val picker by rememberUpdatedState(pick)
    val find by rememberUpdatedState(rowAt)
    var coords by remember { mutableStateOf<LayoutCoordinates?>(null) }
    return this
        .onGloballyPositioned { coords = it }
        .pointerInput(key) {
            detectTapGestures { at -> find(at, size)?.let { commit(picker(it)) } }
        }
        .pointerInput(key) {
            var row = -1
            var last: Picked? = null
            fun show(at: Offset) {
                val r = find(at, size) ?: return
                if (r != row) {
                    row = r
                    haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                    last = picker(r)
                }
                // The card goes in the half of the screen away from the finger.
                val y = coords?.takeIf { it.isAttached }?.localToRoot(at)?.y ?: 0f
                last?.let { sweep(Sweep(it, top = y > screenH / 2)) }
            }
            detectDragGesturesAfterLongPress(
                onDragStart = { row = -1; last = null; show(it) },
                onDrag = { change, _ -> change.consume(); show(change.position) },
                onDragEnd = {
                    sweep(null)
                    last?.let { commit(it) }
                },
                onDragCancel = { sweep(null) },
            )
        }
}

/** A sweep's light card: the commit's id, subject, who and when, branches and what it is. */
@Composable
fun SweepCard(s: Sweep, modifier: Modifier = Modifier) {
    val c = LocalColors.current
    val p = s.picked
    Column(
        modifier.fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 16.dp)
            .background(c.surface, RoundedCornerShape(14.dp))
            .border(1.dp, Color(p.color), RoundedCornerShape(14.dp))
            .padding(14.dp),
    ) {
        Row {
            Text(p.commit.id.take(7), color = Color(p.color), fontFamily = FontFamily.Monospace, fontSize = 14.sp)
            Spacer(Modifier.width(8.dp))
            Text(p.repo.name, color = c.muted, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        Text(
            p.commit.subject,
            color = c.text,
            fontWeight = FontWeight.SemiBold,
            fontSize = 15.sp,
            maxLines = 3,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            listOf(p.commit.author.ifEmpty { "someone" }, ago(p.commit.time * 1000)).joinToString(" · "),
            color = c.muted,
            fontSize = 13.sp,
        )
        val parts = p.refs + listOfNotNull(p.what.ifEmpty { null })
        if (parts.isNotEmpty()) Text(parts.joinToString(" · "), color = c.muted, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}
