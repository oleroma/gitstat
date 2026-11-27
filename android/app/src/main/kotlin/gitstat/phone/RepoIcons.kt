package gitstat.phone

import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.PorterDuff
import android.graphics.RectF
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import com.caverock.androidsvg.SVG
import gitstat.core.Repo

/**
 * Each repo's own icon, `.gitstat/icon.svg`, drawn in its status colour as on the PC
 * (src/repo_icon.rs): every `currentColor` takes the colour, and an icon with none is drawn as
 * a silhouette in it. The file comes from the repo, so it's untrusted: AndroidSVG loads nothing
 * it refers to (no files, fonts or network) without a resolver, and none is set. A repo
 * without an icon, or one that can't be drawn, gets its status dot.
 */
object RepoIcons {
    private val cache = LruCache<String, Bitmap>(96)
    private val CURRENT = Regex("currentColor", RegexOption.IGNORE_CASE)

    fun bitmap(repo: Repo, color: Int, px: Int): Bitmap? {
        val text = repo.icon ?: return null
        if (px <= 0) return null
        val key = "${repo.slug}|${text.hashCode()}|$color|$px"
        cache.get(key)?.let { return it }
        val bmp = try {
            render(text, color, px)
        } catch (e: Exception) {
            null
        } ?: return null
        cache.put(key, bmp)
        return bmp
    }

    private fun render(text: String, color: Int, px: Int): Bitmap {
        val hex = String.format(java.util.Locale.ROOT, "#%06X", color and 0xFFFFFF)
        val spelled = CURRENT.containsMatchIn(text)
        val svg = SVG.getFromString(if (spelled) CURRENT.replace(text, hex) else text)
        if (svg.documentViewBox == null) {
            val w = svg.documentWidth.takeIf { it > 0 } ?: px.toFloat()
            val h = svg.documentHeight.takeIf { it > 0 } ?: px.toFloat()
            svg.setDocumentViewBox(0f, 0f, w, h)
        }
        svg.setDocumentWidth("100%")
        svg.setDocumentHeight("100%")
        val bmp = Bitmap.createBitmap(px, px, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bmp)
        svg.renderToCanvas(canvas, RectF(0f, 0f, px.toFloat(), px.toFloat()))
        if (!spelled) canvas.drawColor(color or 0xFF000000.toInt(), PorterDuff.Mode.SRC_IN)
        return bmp
    }
}

/** The repo's icon at [size] in its status colour, or its status dot. */
@Composable
fun RepoMark(repo: Repo, size: Dp) {
    val c = LocalColors.current
    val color = c.of(repo.health)
    val px = with(LocalDensity.current) { size.roundToPx() }
    val bmp = remember(repo.slug, repo.icon, color, px) { RepoIcons.bitmap(repo, color.toArgb(), px) }
    if (bmp != null) {
        Image(bmp.asImageBitmap(), contentDescription = null, modifier = Modifier.size(size))
    } else {
        Box(Modifier.size(size), contentAlignment = androidx.compose.ui.Alignment.Center) {
            Box(Modifier.size(size * 0.6f).background(color, CircleShape))
        }
    }
}
