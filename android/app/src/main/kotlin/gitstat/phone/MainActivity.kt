package gitstat.phone

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.text.format.DateUtils
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import gitstat.core.Branch
import gitstat.core.Checks
import gitstat.core.Pr
import gitstat.core.Repo
import gitstat.core.Snapshot
import kotlinx.coroutines.launch
import java.time.Instant

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        Hub.load(this)
        setContent { GitstatTheme { Screen() } }
    }
}

@Composable
private fun Screen() {
    val state by Hub.state.collectAsState()
    if (state.signedIn) RepoScreen(state) else SignIn(state.problem)
}

/** How long ago [millis] was, such as "5 minutes ago". */
private fun ago(millis: Long): String =
    DateUtils.getRelativeTimeSpanString(millis, System.currentTimeMillis(), DateUtils.MINUTE_IN_MILLIS).toString()

private fun ago(iso: String): String =
    runCatching { ago(Instant.parse(iso).toEpochMilli()) }.getOrDefault("")

// ---------------------------------------------------------------- sign in

@Composable
private fun SignIn(problem: String?) {
    val c = LocalColors.current
    val context = LocalContext.current
    val uri = LocalUriHandler.current
    val scope = rememberCoroutineScope()
    var token by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf(problem) }

    Scaffold(containerColor = c.bg) { pad ->
        Column(
            Modifier.fillMaxSize().padding(pad).padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text("gitstat", fontSize = 28.sp, fontWeight = FontWeight.Bold, color = c.text)
            Text(
                "Your GitHub repos at a glance. A repo is red when a branch has commits that " +
                    "aren't on main yet, and green when everything is merged.",
                color = c.muted,
            )
            Text(
                "Sign in with a fine-grained GitHub token that can only read. On GitHub, give it " +
                    "access to all repositories and read-only Contents, Pull requests and " +
                    "Commit statuses. The token stays on this phone, encrypted.",
                color = c.muted,
            )
            TextButton(onClick = { uri.openUri(TOKEN_URL) }) { Text("Make a token on GitHub") }
            OutlinedTextField(
                value = token,
                onValueChange = { token = it; error = null },
                label = { Text("Token") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                modifier = Modifier.fillMaxWidth(),
            )
            error?.let { Text(it, color = c.red) }
            Button(
                enabled = token.isNotBlank() && !busy,
                onClick = {
                    busy = true
                    scope.launch {
                        error = Hub.signIn(context, token)
                        busy = false
                    }
                },
            ) { Text(if (busy) "Reading your repos…" else "Sign in") }
        }
    }
}

private const val TOKEN_URL =
    "https://github.com/settings/personal-access-tokens/new?name=gitstat%20phone&description=Read-only%20for%20gitstat%20on%20the%20phone"

// ---------------------------------------------------------------- repos

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RepoScreen(state: UiState) {
    val c = LocalColors.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var menu by remember { mutableStateOf(false) }
    var sheet by remember { mutableStateOf<String?>(null) }
    var picked by remember { mutableStateOf<Picked?>(null) }

    val askNotify = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (!granted) Hub.setNotify(context, false)
    }
    LaunchedEffect(Unit) {
        if (state.notify && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            askNotify.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        val read = state.snapshot?.readAt ?: 0
        if (System.currentTimeMillis() - read > 60_000) Hub.refresh(context)
    }

    val menuButton: @Composable () -> Unit = {
        Box {
            TextButton(onClick = { menu = true }) { Text("⋮", fontSize = 20.sp, color = c.text) }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(
                    text = { Text(if (state.notify) "Turn notifications off" else "Turn notifications on") },
                    onClick = {
                        menu = false
                        val on = !state.notify
                        Hub.setNotify(context, on)
                        if (on && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                            askNotify.launch(Manifest.permission.POST_NOTIFICATIONS)
                        }
                    },
                )
                DropdownMenuItem(
                    text = { Text("Sign out") },
                    onClick = { menu = false; scope.launch { Hub.signOut(context) } },
                )
            }
        }
    }

    Scaffold(
        containerColor = c.bg,
    ) { pad ->
        PullToRefreshBox(
            isRefreshing = state.refreshing,
            onRefresh = { scope.launch { Hub.refresh(context) } },
            modifier = Modifier.fillMaxSize().padding(pad),
        ) {
            val snap = state.snapshot
            val summary: @Composable () -> Unit = {
                Box(Modifier.padding(horizontal = 12.dp, vertical = 4.dp)) { Summary(snap, state.problem, state.refreshing) }
            }
            if (snap == null) {
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    Row(Modifier.fillMaxWidth().padding(start = 16.dp), verticalAlignment = Alignment.CenterVertically) {
                        Text("gitstat", fontWeight = FontWeight.Bold, fontSize = 20.sp, color = c.text, modifier = Modifier.weight(1f))
                        menuButton()
                    }
                    summary()
                }
            } else {
                Board(snap, menu = menuButton, summary = summary, onRepo = { sheet = it.slug }, onCommit = { picked = it })
            }
        }
    }

    val snap = state.snapshot
    val repo = snap?.repos?.firstOrNull { it.slug == sheet }
    if (snap != null && repo != null) {
        ModalBottomSheet(onDismissRequest = { sheet = null }, containerColor = c.surface) {
            Column(Modifier.verticalScroll(rememberScrollState()).padding(start = 16.dp, end = 16.dp, bottom = 24.dp)) {
                RepoCard(repo, snap.login, expanded = true, onToggle = {})
            }
        }
    }
    picked?.let { p ->
        ModalBottomSheet(onDismissRequest = { picked = null }, containerColor = c.surface) {
            CommitCard(p)
        }
    }
}

@Composable
private fun CommitCard(p: Picked) {
    val c = LocalColors.current
    val uri = LocalUriHandler.current
    Column(Modifier.fillMaxWidth().padding(start = 20.dp, end = 20.dp, bottom = 28.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(p.commit.subject, color = c.text, fontWeight = FontWeight.SemiBold)
        val meta = listOfNotNull(
            p.repo.name,
            p.branch,
            p.commit.author.ifEmpty { null },
            ago(p.commit.time * 1000),
            p.commit.id.take(7),
        )
        Text(meta.joinToString(" · "), color = c.muted, fontSize = 13.sp)
        TextButton(onClick = { uri.openUri("${p.repo.url}/commit/${p.commit.id}") }) { Text("Open on GitHub") }
    }
}

@Composable
private fun Summary(snap: Snapshot?, problem: String?, refreshing: Boolean) {
    val c = LocalColors.current
    Column(Modifier.padding(horizontal = 4.dp, vertical = 4.dp)) {
        when {
            snap == null && refreshing -> Text("Reading your repos…", color = c.muted)
            snap == null -> Text("Pull down to read your repos.", color = c.muted)
            else -> {
                val red = snap.redCount
                Text(
                    if (red == 0) "All ${snap.repos.size} repos are merged" else "$red of ${snap.repos.size} repos have branches not merged",
                    color = if (red == 0) c.green else c.red,
                    fontWeight = FontWeight.SemiBold,
                )
                Text("Read ${ago(snap.readAt)}", color = c.muted, fontSize = 12.sp)
            }
        }
        problem?.let { Text(it, color = c.amber, fontSize = 13.sp, modifier = Modifier.padding(top = 4.dp)) }
    }
}

@Composable
private fun Dot(color: Color, size: Int = 12) {
    Box(Modifier.size(size.dp).background(color, CircleShape))
}

@Composable
private fun RepoCard(repo: Repo, login: String, expanded: Boolean, onToggle: () -> Unit) {
    val c = LocalColors.current
    val uri = LocalUriHandler.current
    val hanging = repo.hanging
    Column(
        Modifier.fillMaxWidth()
            .background(c.surface, RoundedCornerShape(12.dp))
            .border(1.dp, c.border, RoundedCornerShape(12.dp))
            .clickable(onClick = onToggle)
            .padding(horizontal = 14.dp, vertical = 12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Dot(c.of(repo.health))
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    if (repo.owner != login) Text("${repo.owner}/", color = c.muted, maxLines = 1)
                    Text(
                        repo.name,
                        color = c.text,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Text(subtitle(repo), color = c.muted, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            ChecksMark(repo.checksState)
        }
        if (expanded) {
            Spacer(Modifier.height(10.dp))
            repo.problem?.let { Text(it, color = c.amber, fontSize = 13.sp) }
            if (hanging.isNotEmpty()) {
                Text("Not merged", color = c.muted, fontSize = 12.sp)
                hanging.forEach { BranchRow(it, c.red) }
            }
            val merged = repo.branches.filter { !it.hanging }
            if (merged.isNotEmpty()) {
                Spacer(Modifier.height(6.dp))
                Text("Merged into ${repo.defaultBranch}", color = c.muted, fontSize = 12.sp)
                merged.forEach { BranchRow(it, c.green) }
            }
            val loose = repo.openPrs.filter { pr -> repo.branches.none { it.pr == pr } }
            if (loose.isNotEmpty()) {
                Spacer(Modifier.height(6.dp))
                Text("Open pull requests", color = c.muted, fontSize = 12.sp)
                loose.forEach { PrRow(it) }
            }
            if (repo.branchCount > repo.branches.size + 1) {
                Text(
                    "Showing the newest ${repo.branches.size} of ${repo.branchCount - 1} branches",
                    color = c.muted,
                    fontSize = 12.sp,
                )
            }
            TextButton(onClick = { uri.openUri(repo.url) }) { Text("Open on GitHub") }
        }
    }
}

private fun subtitle(repo: Repo): String {
    val parts = mutableListOf<String>()
    val n = repo.hanging.size
    parts += when {
        repo.problem != null -> "couldn't read"
        n > 0 -> "${branchCount(n)} not merged"
        else -> "all merged"
    }
    if (repo.openPrs.isNotEmpty()) parts += if (repo.openPrs.size == 1) "1 PR" else "${repo.openPrs.size} PRs"
    ago(repo.pushedAt).takeIf { it.isNotEmpty() }?.let { parts += "pushed $it" }
    return parts.joinToString(" · ")
}

@Composable
private fun ChecksMark(checks: Checks) {
    val c = LocalColors.current
    val (mark, color) = when (checks) {
        Checks.PASSING -> "✓" to c.green
        Checks.FAILING -> "✗" to c.red
        Checks.PENDING -> "●" to c.amber
        Checks.NONE -> return
    }
    Text(mark, color = color, fontSize = 16.sp, modifier = Modifier.padding(start = 8.dp))
}

@Composable
private fun BranchRow(b: Branch, color: Color) {
    val c = LocalColors.current
    val uri = LocalUriHandler.current
    Row(
        Modifier.fillMaxWidth().clickable { uri.openUri(b.pr?.url ?: b.url) }.padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Dot(color, 8)
        Spacer(Modifier.width(10.dp))
        Text(
            b.name,
            color = c.text,
            fontFamily = FontFamily.Monospace,
            fontSize = 13.sp,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        b.pr?.let { PrChip(it) }
        val counts = buildList {
            if (b.hanging) add("+${b.notInMain}")
            if (b.mergedByPr) add("PR merged")
            if (b.mainAhead > 0) add("${b.mainAhead} behind")
        }.joinToString(" ")
        if (counts.isNotEmpty()) Text(counts, color = c.muted, fontSize = 12.sp, modifier = Modifier.padding(start = 8.dp))
    }
}

@Composable
private fun PrRow(pr: Pr) {
    val c = LocalColors.current
    val uri = LocalUriHandler.current
    Row(
        Modifier.fillMaxWidth().clickable { uri.openUri(pr.url) }.padding(vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        PrChip(pr)
        Spacer(Modifier.width(8.dp))
        Text(pr.title, color = c.text, fontSize = 13.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

/** A PR number in gitstat's PR colours: open green, draft grey. */
@Composable
private fun PrChip(pr: Pr) {
    val c = LocalColors.current
    val color = if (pr.draft) c.muted else c.green
    Text(
        "#${pr.number}" + if (pr.draft) " draft" else "",
        color = color,
        fontSize = 12.sp,
        style = MaterialTheme.typography.labelMedium,
        modifier = Modifier
            .padding(start = 6.dp)
            .border(1.dp, color, RoundedCornerShape(10.dp))
            .padding(horizontal = 6.dp, vertical = 1.dp),
    )
}
