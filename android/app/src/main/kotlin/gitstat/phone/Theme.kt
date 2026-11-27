package gitstat.phone

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import gitstat.core.Health

/** gitstat's colours on the PC (src/view.rs and the GitHub Primer PR colours), light and dark. */
object Palette {
    val redLight = Color(0xFFCF222E)
    val redDark = Color(0xFFF26B6B)
    val greenLight = Color(0xFF1A7F37)
    val greenDark = Color(0xFF3FB950)
    val amberLight = Color(0xFF9A6700)
    val amberDark = Color(0xFFD6A33B)
    val purpleLight = Color(0xFF8250DF)
    val purpleDark = Color(0xFFA371F7)
}

@Immutable
data class Colors(
    val bg: Color,
    val surface: Color,
    val border: Color,
    val text: Color,
    val muted: Color,
    val red: Color,
    val green: Color,
    val amber: Color,
    val purple: Color,
) {
    fun of(h: Health) = when (h) {
        Health.RED -> red
        Health.GREEN -> green
        Health.UNKNOWN -> muted
    }
}

private val light = Colors(
    bg = Color(0xFFF6F7F9),
    surface = Color(0xFFFFFFFF),
    border = Color(0xFFDCDFE5),
    text = Color(0xFF1F2328),
    muted = Color(0xFF656D76),
    red = Palette.redLight,
    green = Palette.greenLight,
    amber = Palette.amberLight,
    purple = Palette.purpleLight,
)

private val dark = Colors(
    bg = Color(0xFF1E1F22),
    surface = Color(0xFF2B2D31),
    border = Color(0xFF3A3C42),
    text = Color(0xFFE8E8EA),
    muted = Color(0xFF9A9CA3),
    red = Palette.redDark,
    green = Palette.greenDark,
    amber = Palette.amberDark,
    purple = Palette.purpleDark,
)

val LocalColors = staticCompositionLocalOf { light }

@Composable
fun GitstatTheme(content: @Composable () -> Unit) {
    val isDark = isSystemInDarkTheme()
    val c = if (isDark) dark else light
    val scheme = if (isDark) {
        darkColorScheme(
            primary = Color(0xFF3D6FE0),
            background = c.bg,
            surface = c.bg,
            onBackground = c.text,
            onSurface = c.text,
            surfaceVariant = c.surface,
            onSurfaceVariant = c.muted,
            outline = c.border,
        )
    } else {
        lightColorScheme(
            primary = Color(0xFF2F5FD0),
            background = c.bg,
            surface = c.bg,
            onBackground = c.text,
            onSurface = c.text,
            surfaceVariant = c.surface,
            onSurfaceVariant = c.muted,
            outline = c.border,
        )
    }
    androidx.compose.runtime.CompositionLocalProvider(LocalColors provides c) {
        MaterialTheme(colorScheme = scheme, content = content)
    }
}
