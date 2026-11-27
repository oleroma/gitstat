package gitstat.phone

import androidx.compose.foundation.Canvas
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import gitstat.core.Commit
import gitstat.core.Repo
import gitstat.core.Snapshot
import gitstat.core.Tree
import gitstat.core.tree

/** Rows and lanes of the columns' trees. */
private val ROW = 32.dp
private val LANE = 16.dp
private val DOT = 5.dp

/** A commit picked on a tree, for the card at the bottom of the screen. */
data class Picked(val repo: Repo, val commit: Commit, val branch: String?)

/**
 * One repo per screen, its tree filling the width: swipe sideways for the next repo (red ones
 * first), scroll down through the repo's history. Each page has the repo's dot, name and
 * state on top; tapping it opens the repo's branches and pull requests.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun Board(
    snap: Snapshot,
    header: @Composable () -> Unit,
    onRepo: (Repo) -> Unit,
    onCommit: (Picked) -> Unit,
) {
    val repos = remember(snap) { snap.sorted() }
    val trees = remember(snap) { repos.associate { it.slug to it.tree() } }
    val pager = rememberPagerState { repos.size }
    Column(Modifier.fillMaxSize()) {
        header()
        HorizontalPager(state = pager, modifier = Modifier.fillMaxSize(), key = { repos[it].slug }) { page ->
            val repo = repos[page]
            RepoPage(repo, trees.getValue(repo.slug), snap.login, "${page + 1} / ${repos.size}", onRepo, onCommit)
        }
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
    Column(
        Modifier.fillMaxSize().padding(horizontal = 8.dp)
            .background(c.surface, RoundedCornerShape(12.dp))
            .border(1.dp, c.border, RoundedCornerShape(12.dp))
            .verticalScroll(rememberScrollState()),
    ) {
        Column(
            Modifier.fillMaxWidth().clickable { onRepo(repo) }.padding(horizontal = 14.dp, vertical = 8.dp),
        ) {
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Box(Modifier.size(12.dp).background(c.of(repo.health), CircleShape))
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

@Composable
private fun TreeCanvas(repo: Repo, tree: Tree, onCommit: (Picked) -> Unit) {
    val c = LocalColors.current
    val dark = isSystemInDarkTheme()
    val inks = remember(dark) { TreeInks(dark) }
    val density = LocalDensity.current
    val rowPx = with(density) { ROW.toPx() }
    val lanes = tree.width.coerceAtMost(MAX_LANES)
    val treeW: Dp = LANE * lanes + 14.dp
    val branchAt = remember(repo) {
        repo.branches.associate { it.tip to it.name } + (repo.defaultTip to repo.defaultBranch)
    }
    Box(
        Modifier.fillMaxWidth().height(ROW * tree.rows + 6.dp).pointerInput(repo) {
            detectTapGestures { at ->
                val row = (at.y / rowPx).toInt()
                if (row in 0 until tree.rows) {
                    val commit = repo.commits[row]
                    onCommit(Picked(repo, commit, branchAt[commit.id]))
                }
            }
        },
    ) {
        Canvas(Modifier.width(treeW).height(ROW * tree.rows + 6.dp)) {
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
            }
        }
        // Each commit's subject beside its dot, one per row.
        Column(Modifier.padding(start = treeW)) {
            for (commit in repo.commits) {
                Box(Modifier.height(ROW).padding(end = 8.dp), contentAlignment = androidx.compose.ui.Alignment.CenterStart) {
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
