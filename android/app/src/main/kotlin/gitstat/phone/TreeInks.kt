package gitstat.phone

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import gitstat.core.Ink
import gitstat.core.Tree

/** The tree colours of the PC app (src/timeline.rs `Colors`), as ARGB, light or dark. */
class TreeInks(dark: Boolean) {
    val trunk: Int
    val merged: Int
    val prOpen: Int
    val prDraft: Int
    val hues: IntArray

    init {
        if (dark) {
            trunk = 0xFF8B949E.toInt()
            merged = 0xFFA371F7.toInt()
            prOpen = 0xFF3FB950.toInt()
            prDraft = 0xFF6E7681.toInt()
            // blue, orange, pink, lemon, teal, coral
            hues = intArrayOf(0xFF4493F8.toInt(), 0xFFF0883E.toInt(), 0xFFF778BA.toInt(), 0xFFE3B341.toInt(), 0xFF39C5CF.toInt(), 0xFFFF7B72.toInt())
        } else {
            trunk = 0xFF656D76.toInt()
            merged = 0xFF8250DF.toInt()
            prOpen = 0xFF1A7F37.toInt()
            prDraft = 0xFFAFB8C1.toInt()
            hues = intArrayOf(0xFF0969DA.toInt(), 0xFFBC4C00.toInt(), 0xFFBF3989.toInt(), 0xFF9A6700.toInt(), 0xFF1B7C83.toInt(), 0xFFC4432B.toInt())
        }
    }

    fun node(tree: Tree, row: Int): Int = when (tree.inkOf(row)) {
        Ink.TRUNK -> trunk
        Ink.MERGED -> merged
        Ink.PR_OPEN -> prOpen
        Ink.PR_DRAFT -> prDraft
        Ink.BRANCH -> hues[tree.hueOf(row, hues.size)]
    }

    /** A noodle's colour: the child's along its first parent, the parent's for a merged-in one. */
    fun edge(tree: Tree, e: gitstat.core.Edge): Int =
        if (e.first || e.parentRow == null) node(tree, e.childRow) else node(tree, e.parentRow!!)
}

/** Lanes drawn before the rest fold onto the last one, as on the PC. */
const val MAX_LANES = 6

/**
 * Draws [tree] as a horizontal strip, newest commit at the right edge and history running off
 * to the left, as gitstat's tray strips do: lane 0 (the trunk) on the centre line, the other
 * lanes alternating above and below it. Sizes are in px.
 */
fun stripBitmap(tree: Tree, inks: TreeInks, bg: Int, widthPx: Int, heightPx: Int, density: Float): Bitmap {
    val bmp = Bitmap.createBitmap(widthPx.coerceAtLeast(1), heightPx.coerceAtLeast(1), Bitmap.Config.ARGB_8888)
    val c = Canvas(bmp)
    val step = 11f * density
    val lane = 4.5f * density
    val dot = 2.6f * density
    val right = widthPx - dot - 3f * density
    val mid = heightPx / 2f
    val lanes = (((heightPx / 2f - dot - density) / lane).toInt() * 2 + 1).coerceIn(1, MAX_LANES)
    fun x(row: Float) = right - row * step
    fun y(l: Float): Float {
        val n = l.coerceAtMost((lanes - 1).toFloat())
        val k = kotlin.math.ceil(n / 2f)
        return if (n.toInt() % 2 == 1) mid - k * lane else mid + k * lane
    }
    val visible = ((right / step).toInt() + 2).coerceAtMost(tree.rows)
    val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 1.4f * density
        strokeCap = Paint.Cap.ROUND
    }
    val path = Path()
    for (e in tree.edges) {
        if (e.childRow > visible) continue
        paint.color = inks.edge(tree, e)
        for (s in Tree.segments(e, tree.rows)) {
            val x1 = x(s.row1)
            val y1 = y(s.lane1)
            val x2 = x(s.row2)
            val y2 = y(s.lane2)
            path.reset()
            path.moveTo(x1, y1)
            if (s.curve && y1 != y2) {
                val xm = (x1 + x2) / 2
                path.cubicTo(xm, y1, xm, y2, x2, y2)
            } else {
                path.lineTo(x2, y2)
            }
            c.drawPath(path, paint)
        }
    }
    val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.FILL }
    for (row in 0 until visible) {
        val n = tree.nodes[row]
        val cx = x(row.toFloat())
        val cy = y(n.lane.toFloat())
        fill.color = inks.node(tree, row)
        c.drawCircle(cx, cy, dot, fill)
        if (n.merge) {
            fill.color = bg
            c.drawCircle(cx, cy, dot * 0.45f, fill)
        }
    }
    return bmp
}
