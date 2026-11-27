package gitstat.phone

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.action.actionStartActivity
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import androidx.glance.appwidget.SizeMode
import androidx.glance.appwidget.cornerRadius
import androidx.glance.appwidget.lazy.LazyColumn
import androidx.glance.appwidget.lazy.items
import androidx.glance.appwidget.provideContent
import androidx.glance.background
import androidx.glance.color.ColorProvider
import androidx.glance.layout.Alignment
import androidx.glance.layout.Box
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.padding
import androidx.glance.layout.size
import androidx.glance.layout.width
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.text.TextStyle
import gitstat.core.Health
import gitstat.core.Snapshot

class GitstatWidgetReceiver : GlanceAppWidgetReceiver() {
    override val glanceAppWidget: GlanceAppWidget = GitstatWidget()
}

/** The home-screen widget: every repo's dot and name, red ones first. Tapping opens the app. */
class GitstatWidget : GlanceAppWidget() {
    override val sizeMode = SizeMode.Single

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

@Composable
private fun WidgetBody(snap: Snapshot?, signedIn: Boolean) {
    Column(
        GlanceModifier.fillMaxSize().background(bg).cornerRadius(16.dp).padding(12.dp)
            .clickable(actionStartActivity<MainActivity>()),
    ) {
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
        if (snap != null && signedIn) {
            LazyColumn(GlanceModifier.fillMaxSize().padding(top = 6.dp)) {
                items(snap.sorted()) { repo ->
                    Row(
                        GlanceModifier.fillMaxWidth().padding(vertical = 3.dp)
                            .clickable(actionStartActivity<MainActivity>()),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Box(GlanceModifier.size(10.dp).cornerRadius(5.dp).background(dot(repo.health))) {}
                        Spacer(GlanceModifier.width(8.dp))
                        Text(
                            repo.name,
                            style = TextStyle(color = text, fontSize = 13.sp),
                            maxLines = 1,
                            modifier = GlanceModifier.defaultWeight(),
                        )
                        val n = repo.hanging.size
                        if (n > 0) Text("$n", style = TextStyle(color = dot(Health.RED), fontSize = 12.sp))
                    }
                }
            }
        }
    }
}
