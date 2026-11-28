package gitstat.phone

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import gitstat.core.Repo
import gitstat.core.Snapshot
import gitstat.core.Tree
import gitstat.core.tree

/** Rows and lanes of the columns' trees. */
private val ROW = 32.dp
private val LANE = 16.dp
private val DOT = 5.dp


/**
 * One repo per screen, its tree filling the width: swipe sideways for the next repo (red ones
 * first), scroll down through the repo's history. Each page has the repo's dot, name and
 * state on top; tapping it opens the repo's branches and pull requests.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun Board(
    snap: Snapshot,
    /** The repo to open at, such as one tapped in the horizontal view. */
    focus: String?,
    problem: String?,
    menu: @Composable () -> Unit,
    onHealth: () -> Unit,
    /** A repo's tab was tapped: back to the list of rows, that repo lit. */
    onTab: (Repo) -> Unit,
    onRepo: (Repo) -> Unit,
    onCommit: (Picked) -> Unit,
) {
    val repos = remember(snap) { snap.sorted() }
    val trees = remember(snap) { repos.associate { it.slug to it.tree() } }
    val start = remember(snap, focus) { repos.indexOfFirst { it.slug == focus }.coerceAtLeast(0) }
    val pager = rememberPagerState(initialPage = start) { repos.size }
    LaunchedEffect(focus) { if (focus != null) pager.scrollToPage(start) }
    val pills = rememberLazyListState()
    // The pill of the repo on screen stays in view as you swipe.
    LaunchedEffect(pager.currentPage) {
        if (repos.isNotEmpty()) pills.animateScrollToItem((pager.currentPage - 1).coerceAtLeast(0))
    }
    Column(Modifier.fillMaxSize()) {
        // The view switch and the menu on a row of their own, top right; the pills below get the whole width.
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) { menu() }
        Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
            LazyRow(
                state = pills,
                modifier = Modifier.fillMaxWidth(),
                contentPadding = PaddingValues(horizontal = 8.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                item(key = "health") { HealthBadge(snap, problem, onHealth) }
                itemsIndexed(repos, key = { _, r -> r.slug }) { i, repo ->
                    Pill(repo, selected = i == pager.currentPage) { onTab(repo) }
                }
            }
        }
        HorizontalPager(state = pager, modifier = Modifier.fillMaxSize(), key = { repos[it].slug }) { page ->
            val repo = repos[page]
            RepoPage(repo, trees.getValue(repo.slug), snap.login, "${page + 1} / ${repos.size}", onRepo, onCommit)
        }
    }
}

/**
 * How many repos have branches not merged, as a number in a circle: red, or green at 0, with
 * an amber ring when the last read failed. Tapping it opens the to-do list across all repos.
 */
@Composable
fun HealthBadge(snap: Snapshot, problem: String?, onClick: () -> Unit) {
    val c = LocalColors.current
    val red = snap.redCount
    val fill = if (red > 0) c.red else c.green
    val density = LocalDensity.current
    // Grows with the font size, so the number always fits.
    val d = 32.dp * maxOf(1f, density.fontScale)
    Box(
        Modifier.size(d)
            .background(fill, CircleShape)
            .then(if (problem != null) Modifier.border(2.dp, c.amber, CircleShape) else Modifier)
            .clip(CircleShape)
            .clickable(onClick = onClick),
        contentAlignment = androidx.compose.ui.Alignment.Center,
    ) {
        Text("$red", color = Color.White, fontWeight = FontWeight.Bold, fontSize = 14.sp, maxLines = 1)
    }
}

/** A repo's tab: its icon and name in a pill; the repo on screen has its pill filled. */
@Composable
private fun Pill(repo: Repo, selected: Boolean, onClick: () -> Unit) {
    val c = LocalColors.current
    val shape = RoundedCornerShape(50)
    Row(
        Modifier
            .background(if (selected) c.text.copy(alpha = 0.12f) else c.surface, shape)
            .border(1.dp, if (selected) c.of(repo.health) else c.border, shape)
            .clip(shape)
            .clickable(onClick = onClick)
            .padding(start = 8.dp, end = 12.dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        RepoMark(repo, 18.dp)
        Spacer(Modifier.width(6.dp))
        Text(
            repo.name,
            color = c.text,
            fontSize = 14.sp,
            fontWeight = if (selected) FontWeight.SemiBold else FontWeight.Normal,
            maxLines = 1,
        )
    }
}

@Composable
private fun RepoPage(
    repo: Repo,
    tree: Tree,
    login: String,
    position: String,
    onRepo: (Repo) -> Unit,
    onCommit: (Picked) -> Unit,
) {
    val c = LocalColors.current
    // The repo's info sits above its header: the page rests with it scrolled out of sight
    // and, pulled down past the top, snaps with it in full view.
    val scroll = rememberScrollState()
    var infoH by remember { mutableIntStateOf(0) }
    var ready by remember { mutableStateOf(false) }
    LaunchedEffect(infoH) {
        if (infoH > 0 && !ready) {
            scroll.scrollTo(infoH)
            ready = true
        }
    }
    val fling = peekFling(scroll, rest = infoH, info = 0, historyBelow = false)
    BoxWithConstraints(Modifier.fillMaxSize()) {
    // At least a screen below the info, so a short tree still rests with the info hidden.
    val page = maxHeight
    Column(
        Modifier.fillMaxSize().padding(horizontal = 8.dp)
            .background(c.surface, RoundedCornerShape(12.dp))
            .border(1.dp, c.border, RoundedCornerShape(12.dp))
            .clip(RoundedCornerShape(12.dp))
            // Hidden until it rests below the info, so the info never flashes up.
            .graphicsLayer { alpha = if (ready) 1f else 0f }
            .verticalScroll(scroll, flingBehavior = fling),
    ) {
        Column(Modifier.fillMaxWidth().onSizeChanged { infoH = it.height }) {
            RepoFacts(repo, login, compact = false, modifier = Modifier.fillMaxWidth().padding(14.dp))
            Box(Modifier.fillMaxWidth().height(1.dp).background(c.border))
        }
        Column(Modifier.fillMaxWidth().heightIn(min = page)) {
        Column(
            Modifier.fillMaxWidth().clickable { onRepo(repo) }.padding(horizontal = 14.dp, vertical = 8.dp),
        ) {
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                RepoMark(repo, 24.dp)
                Spacer(Modifier.width(10.dp))
                Text(
                    repo.name,
                    color = c.text,
                    fontWeight = FontWeight.SemiBold,
                    fontSize = 17.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                Text(position, color = c.muted, fontSize = 12.sp)
            }
            val n = repo.hanging.size
            Text(
                when {
                    repo.problem != null -> "couldn't read"
                    n > 0 -> "${branchCount(n)} not merged: " + repo.hanging.joinToString(", ") { it.name }
                    else -> "all merged"
                } + if (repo.owner != login) " · ${repo.owner}" else "",
                color = if (n > 0) c.red else c.muted,
                fontSize = 13.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        Box(Modifier.fillMaxWidth().height(1.dp).background(c.border))
        if (repo.commits.isEmpty()) {
            Text("No commits read yet. Pull down to refresh.", color = c.muted, fontSize = 13.sp, modifier = Modifier.padding(14.dp))
        } else {
            TreeCanvas(repo, tree, onCommit)
        }
        Spacer(Modifier.height(24.dp))
        }
    }
    }
}

@Composable
private fun TreeCanvas(repo: Repo, tree: Tree, onCommit: (Picked) -> Unit) {
    val c = LocalColors.current
    val dark = isSystemInDarkTheme()
    val inks = remember(dark) { TreeInks(dark) }
    val density = LocalDensity.current
    // Rows grow with the phone's font size, so subjects are never cut.
    val row = ROW * maxOf(1f, density.fontScale)
    val rowPx = with(density) { row.toPx() }
    val lanes = tree.width.coerceAtMost(MAX_LANES)
    val treeW: Dp = LANE * lanes + 14.dp
    // The picked commit's row is tinted and its dot ringed.
    val sel = LocalTouch.current.selected?.takeIf { it.first == repo.slug }?.second
    val selRow = repo.commits.indexOfFirst { it.id == sel }
    val selColor = if (selRow >= 0) Color(inks.node(tree, selRow)) else Color.Transparent
    Box(
        Modifier.fillMaxWidth().height(row * tree.rows + 6.dp)
            .drawBehind {
                if (selRow >= 0) {
                    drawRect(selColor.copy(alpha = 0.16f), Offset(0f, selRow * rowPx), Size(size.width, rowPx))
                }
            }
            .commitTouches(
                repo,
                rowAt = { at, _ -> (at.y / rowPx).toInt().takeIf { it in 0 until tree.rows } },
                pick = { pick(repo, tree, it, inks) },
                onCommit = onCommit,
            ),
    ) {
        Canvas(Modifier.width(treeW).height(row * tree.rows + 6.dp)) {
            val lanePx = LANE.toPx()
            val x0 = 7.dp.toPx() + lanePx / 2
            fun x(l: Float) = x0 + l.coerceAtMost((lanes - 1).toFloat()) * lanePx
            fun y(r: Float) = r * rowPx + rowPx / 2
            val stroke = Stroke(width = 1.6.dp.toPx(), cap = StrokeCap.Round)
            for (e in tree.edges) {
                val color = Color(inks.edge(tree, e))
                for (s in Tree.segments(e, tree.rows)) {
                    val a = Offset(x(s.lane1), y(s.row1))
                    val b = Offset(x(s.lane2), y(s.row2))
                    if (s.curve && a.x != b.x) {
                        val ym = (a.y + b.y) / 2
                        val p = Path().apply {
                            moveTo(a.x, a.y)
                            cubicTo(a.x, ym, b.x, ym, b.x, b.y)
                        }
                        drawPath(p, color, style = stroke)
                    } else {
                        drawLine(color, a, b, strokeWidth = stroke.width, cap = StrokeCap.Round)
                    }
                }
            }
            val r = DOT.toPx()
            for (row in 0 until tree.rows) {
                val n = tree.nodes[row]
                val at = Offset(x(n.lane.toFloat()), y(row.toFloat()))
                drawCircle(Color(inks.node(tree, row)), r, at)
                if (n.merge) drawCircle(c.surface, r * 0.45f, at)
                if (row == selRow) drawCircle(selColor, r * 1.9f, at, style = Stroke(1.6.dp.toPx()))
            }
        }
        // Each commit's subject beside its dot, one per row.
        Column(Modifier.padding(start = treeW)) {
            for (commit in repo.commits) {
                Box(Modifier.height(row).padding(end = 8.dp), contentAlignment = androidx.compose.ui.Alignment.CenterStart) {
                    Text(
                        commit.subject,
                        color = c.text,
                        fontSize = 13.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}
