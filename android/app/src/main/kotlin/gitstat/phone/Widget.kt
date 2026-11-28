package gitstat.phone

import android.content.Context
import android.content.res.Configuration
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.Image
import androidx.glance.ImageProvider
import androidx.glance.LocalContext
import androidx.glance.LocalSize
import androidx.glance.action.actionStartActivity
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import androidx.glance.appwidget.SizeMode
import androidx.glance.appwidget.cornerRadius
import androidx.glance.appwidget.provideContent
import androidx.glance.background
import androidx.glance.color.ColorProvider
import androidx.glance.layout.Alignment
import androidx.glance.layout.Box
import androidx.glance.layout.Column
import androidx.glance.layout.ContentScale
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.height
import androidx.glance.layout.padding
import androidx.glance.layout.size
import androidx.glance.layout.width
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.text.TextAlign
import androidx.glance.text.TextStyle
import androidx.glance.appwidget.lazy.LazyColumn
import androidx.glance.appwidget.lazy.items
import gitstat.core.Health
import gitstat.core.Snapshot
import gitstat.core.tree

class GitstatWidgetReceiver : GlanceAppWidgetReceiver() {
    override val glanceAppWidget: GlanceAppWidget = GitstatWidget()
}

/**
 * The home-screen widget: a row per repo, red ones first, each with its dot, its name and its
 * tree as a horizontal strip (newest commit at the right, as in gitstat's tray). As many rows
 * as fit; tapping opens the app.
 */
class GitstatWidget : GlanceAppWidget() {
    override val sizeMode = SizeMode.Exact

    override suspend fun provideGlance(context: Context, id: GlanceId) {
        val store = Store(context)
        val snap = store.snapshot()
        val signedIn = store.token != null
        provideContent { WidgetBody(snap, signedIn) }
    }
}

private val bg = ColorProvider(day = Color(0xFFFFFFFF), night = Color(0xFF2B2D31))
private val text = ColorProvider(day = Color(0xFF1F2328), night = Color(0xFFE8E8EA))
private val muted = ColorProvider(day = Color(0xFF656D76), night = Color(0xFF9A9CA3))

private fun dot(h: Health) = when (h) {
    Health.RED -> ColorProvider(day = Palette.redLight, night = Palette.redDark)
    Health.GREEN -> ColorProvider(day = Palette.greenLight, night = Palette.greenDark)
    Health.UNKNOWN -> muted
}

private const val ROW_DP = 26
private const val NAME_DP = 96
private const val STRIP_DP = 22

@Composable
private fun WidgetBody(snap: Snapshot?, signedIn: Boolean) {
    val context = LocalContext.current
    val size = LocalSize.current
    val dark = (context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
        Configuration.UI_MODE_NIGHT_YES
    val density = context.resources.displayMetrics.density
    val inks = TreeInks(dark)
    val bgArgb = if (dark) 0xFF2B2D31.toInt() else 0xFFFFFFFF.toInt()
    val open = actionStartActivity<MainActivity>()

    Column(GlanceModifier.fillMaxSize().background(bg).cornerRadius(16.dp).padding(10.dp).clickable(open)) {
        Row(GlanceModifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text("gitstat", style = TextStyle(color = text, fontWeight = FontWeight.Bold, fontSize = 14.sp))
            Spacer(GlanceModifier.defaultWeight())
            val summary = when {
                !signedIn -> "Tap to sign in"
                snap == null -> "Reading…"
                snap.redCount == 0 -> "all merged"
                else -> "${snap.redCount} not merged"
            }
            Text(summary, style = TextStyle(color = muted, fontSize = 12.sp))
        }
        if (snap == null || !signedIn) return@Column
        // The tree on the left and the name on the right, as on the PC's panel; every repo, scrolling.
        val stripDp = (size.width.value - 20 - NAME_DP - 8 - 16 - 6).coerceAtLeast(20f)
        // Strips are drawn at most 2 px per dp: sharp enough, and light on the launcher's memory.
        val scale = minOf(density, 2f)
        LazyColumn(GlanceModifier.fillMaxSize().padding(top = 4.dp)) {
            items(snap.sorted()) { repo ->
                Row(
                    GlanceModifier.fillMaxWidth().height(ROW_DP.dp).clickable(open),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (repo.commits.isNotEmpty()) {
                        val bmp = stripBitmap(
                            repo.tree(), inks, bgArgb,
                            (stripDp * scale).toInt(), (STRIP_DP * scale).toInt(), scale,
                        )
                        Image(
                            provider = ImageProvider(bmp),
                            contentDescription = null,
                            contentScale = ContentScale.FillBounds,
                            modifier = GlanceModifier.width(stripDp.dp).height(STRIP_DP.dp),
                        )
                    } else {
                        Spacer(GlanceModifier.width(stripDp.dp))
                    }
                    Spacer(GlanceModifier.width(8.dp))
                    val tone = when (repo.health) {
                        Health.RED -> if (dark) Palette.redDark else Palette.redLight
                        Health.GREEN -> if (dark) Palette.greenDark else Palette.greenLight
                        Health.UNKNOWN -> if (dark) Color(0xFF9A9CA3) else Color(0xFF656D76)
                    }
                    val icon = RepoIcons.bitmap(repo, tone.toArgb(), (16 * density).toInt())
                    if (icon != null) {
                        Image(ImageProvider(icon), contentDescription = null, modifier = GlanceModifier.size(16.dp))
                    } else {
                        Box(GlanceModifier.size(16.dp), contentAlignment = Alignment.Center) {
                            Box(GlanceModifier.size(10.dp).cornerRadius(5.dp).background(dot(repo.health))) {}
                        }
                    }
                    Spacer(GlanceModifier.width(6.dp))
                    Text(
                        repo.name,
                        style = TextStyle(color = text, fontSize = 13.sp, textAlign = TextAlign.Start),
                        maxLines = 1,
                        modifier = GlanceModifier.width(NAME_DP.dp),
                    )
                }
            }
        }
    }
}
