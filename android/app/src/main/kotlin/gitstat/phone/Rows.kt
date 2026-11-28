package gitstat.phone

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import gitstat.core.CommitDetail
import gitstat.core.Ink
import gitstat.core.Repo
import gitstat.core.Snapshot
import gitstat.core.Tree
import gitstat.core.tree
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/** Strip geometry, as the PC's panel draws its strips (src/timeline.rs), a little larger for touch. */
private val STEP = 20.dp
private val STRIP_LANE = 7.dp
private val STRIP_DOT = 4.5.dp
private val STRIP_H = 52.dp
private val NAME_W = 116.dp
/** Between the tree and the name, and the margin at the screen's edges. */
private val GAP = 8.dp
private val EDGE = 12.dp
private const val STRIP_LANES = 5

/** What a commit card needs, picked from a tree. */
data class Picked(
    val repo: Repo,
    val commit: gitstat.core.Commit,
    /** Branches whose tip it is. */
    val refs: List<String>,
    /** Its dot's colour. */
    val color: Int,
    /** What its colour says, as on the PC card: "on the default branch", "merged", "PR open"… */
    val what: String,
)

/** The [Picked] for [row] of [repo]'s [tree]. */
fun pick(repo: Repo, tree: Tree, row: Int, inks: TreeInks): Picked {
    val commit = repo.commits[row]
    val refs = buildList {
        if (repo.defaultTip == commit.id) add(repo.defaultBranch)
        repo.branches.filter { it.tip == commit.id }.forEach { add(it.name) }
    }
    val what = when (tree.inkOf(row)) {
        Ink.TRUNK -> "on the default branch"
        Ink.MERGED -> "merged"
        Ink.PR_OPEN -> "PR open"
        Ink.PR_DRAFT -> "PR draft"
        Ink.BRANCH -> if (repo.branches.any { it.tip == commit.id && it.hanging }) "not merged" else ""
    }
    return Picked(repo, commit, refs, inks.node(tree, row), what)
}

/**
 * The horizontal view, like gitstat's panel on the PC: a row per repo, red ones first, each
 * with its icon and name and its tree as a strip, newest commit at the right. The list scrolls
 * down; each strip scrolls sideways through its own history. Tapping a repo's name calls
 * [onRepo] (its page in the vertical view); a long press opens it on GitHub. The repo
 * [highlight] names is lit; scrolling a strip or tapping a commit lights that repo
 * ([onHighlight]). With [jump], the list starts scrolled to the lit repo.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun RowsView(
    snap: Snapshot,
    top: @Composable () -> Unit,
    highlight: String?,
    jump: Boolean,
    onHighlight: (String) -> Unit,
    onRepo: (Repo) -> Unit,
    onCommit: (Picked) -> Unit,
) {
    val c = LocalColors.current
    val uri = LocalUriHandler.current
    val repos = remember(snap) { snap.sorted() }
    val trees = remember(snap) { repos.associate { it.slug to it.tree() } }
    val list = rememberLazyListState()
    LaunchedEffect(jump, highlight) {
        val i = repos.indexOfFirst { it.slug == highlight }
        // Item 0 is the header, so item i is the row above the lit one: it stays in sight too.
        if (jump && i >= 0) list.animateScrollToItem(i)
    }
    LazyColumn(Modifier.fillMaxSize(), state = list, contentPadding = PaddingValues(bottom = 24.dp)) {
        item { top() }
        items(repos, key = { it.slug }) { repo ->
            // As on the PC's panel: the tree on the left running back into history, the
            // newest commit next to the repo's icon and name on the right, icons in a column.
            val lit = repo.slug == highlight
            val tree = trees.getValue(repo.slug)
            // One scroller per row: the tree running back into history on the left, the
            // newest commit next to the repo's icon and name, and past them its info. It rests
            // with the name at the right edge; pushed past the newest commit, it snaps with the
            // icon at the left edge and the info filling the rest of the screen.
            BoxWithConstraints(
                Modifier.fillMaxWidth()
                    .background(if (lit) c.of(repo.health).copy(alpha = 0.14f) else Color.Transparent),
            ) {
                val w = maxWidth
                val view = (w - NAME_W - GAP - EDGE).coerceAtLeast(0.dp)
                val treeW = maxOf(view, STEP * (tree.rows + 1))
                val infoW = (w - NAME_W - EDGE).coerceAtLeast(0.dp)
                val density = LocalDensity.current
                val rest = with(density) { (treeW - view).roundToPx() }
                val info = with(density) { (treeW + GAP - EDGE).roundToPx() }
                val scroll = remember(rest, info) { ScrollState(rest) }
                val fling = peekFling(scroll, rest, info, historyBelow = true)
                LaunchedEffect(scroll.isScrollInProgress) { if (scroll.isScrollInProgress) onHighlight(repo.slug) }
                Row(
                    Modifier.horizontalScroll(scroll, flingBehavior = fling),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    StripCanvas(repo, tree, treeW) { onHighlight(repo.slug); onCommit(it) }
                    Spacer(Modifier.width(GAP))
                    Column(
                        Modifier.width(NAME_W)
                            .combinedClickable(onLongClick = { uri.openUri(repo.url) }) { onRepo(repo) }
                            .padding(vertical = 6.dp),
                    ) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            RepoMark(repo, 20.dp)
                            Spacer(Modifier.width(6.dp))
                            Text(
                                repo.name,
                                color = c.text,
                                fontSize = 14.sp,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.weight(1f, fill = false),
                            )
                        }
                        val n = repo.hanging.size
                        Text(
                            if (repo.problem != null) "couldn't read" else if (n > 0) "$n not merged" else "all merged",
                            color = if (n > 0) c.red else c.muted,
                            fontSize = 11.sp,
                            maxLines = 1,
                            // Under the name, past the icon.
                            modifier = Modifier.padding(start = 26.dp),
                        )
                    }
                    RepoFacts(
                        repo,
                        snap.login,
                        compact = true,
                        // Starts past the screen's edge margin, so none of it shows while the row rests.
                        modifier = Modifier.width(infoW).padding(start = EDGE + 8.dp, end = EDGE),
                    )
                }
            }
            Box(Modifier.fillMaxWidth().padding(end = 12.dp).height(1.dp).background(c.border))
        }
    }
}

/** [repo]'s tree as a strip [width] wide, newest commit at the right end. */
@Composable
private fun StripCanvas(repo: Repo, tree: Tree, width: Dp, onCommit: (Picked) -> Unit) {
    val dark = isSystemInDarkTheme()
    val inks = remember(dark) { TreeInks(dark) }
    val c = LocalColors.current
    val sel = LocalTouch.current.selected?.takeIf { it.first == repo.slug }?.second
    val selRow = repo.commits.indexOfFirst { it.id == sel }
    Canvas(
        Modifier.width(width).height(STRIP_H).commitTouches(
            repo,
            rowAt = { at, size ->
                val right = size.width - STEP.toPx() / 2
                Math.round((right - at.x) / STEP.toPx()).takeIf { it in 0 until tree.rows }
            },
            pick = { pick(repo, tree, it, inks) },
            onCommit = onCommit,
        ),
    ) {
        val step = STEP.toPx()
        val lane = STRIP_LANE.toPx()
        val right = size.width - step / 2
        val mid = size.height / 2
        fun x(r: Float) = right - r * step
        fun y(l: Float): Float {
            val n = l.coerceAtMost((STRIP_LANES - 1).toFloat())
            val k = kotlin.math.ceil(n / 2f)
            return if (n.toInt() % 2 == 1) mid - k * lane else mid + k * lane
        }
        // The picked commit: a band across the strip behind it, and a ring round its dot.
        val selInk = if (selRow >= 0) Color(inks.node(tree, selRow)) else Color.Transparent
        if (selRow >= 0) {
            drawRect(selInk.copy(alpha = 0.16f), Offset(x(selRow.toFloat()) - step / 2, 0f), Size(step, size.height))
        }
        val stroke = Stroke(width = 1.6.dp.toPx(), cap = StrokeCap.Round)
        for (e in tree.edges) {
            val color = Color(inks.edge(tree, e))
            for (s in Tree.segments(e, tree.rows)) {
                val a = Offset(x(s.row1), y(s.lane1))
                val b = Offset(x(s.row2), y(s.lane2))
                if (s.curve && a.y != b.y) {
                    val xm = (a.x + b.x) / 2
                    drawPath(Path().apply { moveTo(a.x, a.y); cubicTo(xm, a.y, xm, b.y, b.x, b.y) }, color, style = stroke)
                } else {
                    drawLine(color, a, b, strokeWidth = stroke.width, cap = StrokeCap.Round)
                }
            }
        }
        val r = STRIP_DOT.toPx()
        for (row in 0 until tree.rows) {
            val n = tree.nodes[row]
            val at = Offset(x(row.toFloat()), y(n.lane.toFloat()))
            drawCircle(Color(inks.node(tree, row)), r, at)
            if (n.merge) drawCircle(c.bg, r * 0.45f, at)
            if (row == selRow) drawCircle(selInk, r * 1.9f, at, style = Stroke(1.6.dp.toPx()))
        }
    }
}

private val UTC = DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm").withZone(ZoneOffset.UTC)

/**
 * A commit's card, as the PC shows one: its short id in its dot's colour and its subject, the
 * rest of its message, who and when, its size, and its branches and what its colour says.
 */
@Composable
fun CommitPopup(p: Picked, onDismiss: () -> Unit) {
    val c = LocalColors.current
    val context = LocalContext.current
    val uri = LocalUriHandler.current
    var detail by remember(p) { mutableStateOf<CommitDetail?>(null) }
    var failed by remember(p) { mutableStateOf(false) }
    LaunchedEffect(p) {
        Hub.commit(context, p.repo, p.commit.id)
            .onSuccess { detail = it }
            .onFailure { failed = true }
    }
    Dialog(onDismissRequest = onDismiss) {
        Column(
            Modifier.fillMaxWidth()
                .background(c.surface, RoundedCornerShape(14.dp))
                .border(1.dp, c.border, RoundedCornerShape(14.dp))
                .padding(16.dp)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Row {
                Text(
                    p.commit.id.take(7),
                    color = Color(p.color),
                    fontFamily = FontFamily.Monospace,
                    fontSize = 14.sp,
                )
                Spacer(Modifier.width(8.dp))
                Text(p.commit.subject, color = c.text, fontWeight = FontWeight.SemiBold, fontSize = 14.sp)
            }
            val body = detail?.message?.substringAfter('\n', "")?.trim().orEmpty()
            if (body.isNotEmpty()) Text(body, color = c.text, fontSize = 13.sp)
            val time = p.commit.time
            Text(
                listOf(
                    p.commit.author.ifEmpty { "someone" },
                    ago(time * 1000),
                    UTC.format(Instant.ofEpochSecond(time)) + " UTC",
                ).joinToString(" · "),
                color = c.muted,
                fontSize = 13.sp,
            )
            val d = detail
            Text(
                when {
                    d?.additions != null && d.deletions != null ->
                        "+${d.additions} −${d.deletions}" + (d.files?.let { " in $it file" + if (it == 1) "" else "s" } ?: "")
                    failed -> "Couldn't read its size"
                    else -> "…"
                },
                color = c.muted,
                fontSize = 13.sp,
            )
            val parts = p.refs + listOfNotNull(p.what.ifEmpty { null })
            if (parts.isNotEmpty()) Text(parts.joinToString(" · "), color = c.muted, fontSize = 13.sp)
            Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
                TextButton(onClick = { copy(context, p.commit.id) }) { Text("Copy id") }
                TextButton(onClick = { uri.openUri("${p.repo.url}/commit/${p.commit.id}") }) { Text("Open on GitHub") }
            }
        }
    }
}

private fun copy(context: Context, text: String) {
    val cm = context.getSystemService(ClipboardManager::class.java) ?: return
    cm.setPrimaryClip(ClipData.newPlainText("commit", text))
}
