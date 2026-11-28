package gitstat.phone

import androidx.compose.foundation.Canvas
import androidx.compose.material3.Checkbox
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
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
import androidx.compose.runtime.saveable.rememberSaveable
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
fun ago(millis: Long): String =
    DateUtils.getRelativeTimeSpanString(millis, System.currentTimeMillis(), DateUtils.MINUTE_IN_MILLIS).toString()

fun ago(iso: String): String =
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
    var health by remember { mutableStateOf(false) }
    // The repo the vertical view opens at, when one was tapped in the horizontal view.
    var focus by rememberSaveable { mutableStateOf<String?>(null) }
    // The repo lit in the horizontal view, and whether that view should scroll to it on opening.
    var lit by rememberSaveable { mutableStateOf<String?>(null) }
    var jump by remember { mutableStateOf(false) }

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

    // Top right: the switch between one repo per screen and a row per repo, then the menu.
    val actions: @Composable () -> Unit = {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TextButton(onClick = { focus = null; Hub.setRows(context, !state.rows) }) {
                Text(if (state.rows) "↕" else "↔", fontSize = 20.sp, color = c.text)
            }
            menuButton()
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
                        actions()
                    }
                    summary()
                }
            } else if (state.rows) {
                RowsView(
                    snap,
                    top = {
                        Row(
                            Modifier.fillMaxWidth().padding(start = 12.dp, top = 4.dp, bottom = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            HealthBadge(snap, state.problem) { health = true }
                            Spacer(Modifier.width(10.dp))
                            Text("gitstat", fontWeight = FontWeight.Bold, fontSize = 20.sp, color = c.text, modifier = Modifier.weight(1f))
                            actions()
                        }
                    },
                    highlight = lit,
                    jump = jump,
                    onHighlight = { lit = it; jump = false },
                    onRepo = { lit = it.slug; jump = false; focus = it.slug; Hub.setRows(context, false) },
                    onCommit = { picked = it },
                )
            } else {
                Board(
                    snap,
                    focus = focus,
                    problem = state.problem,
                    menu = actions,
                    onHealth = { health = true },
                    onTab = { lit = it.slug; jump = true; Hub.setRows(context, true) },
                    onRepo = { sheet = it.slug },
                    onCommit = { picked = it },
                )
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
    if (snap != null && health) {
        ModalBottomSheet(onDismissRequest = { health = false }, containerColor = c.surface) {
            TodoSheet(snap, state.problem)
        }
    }
    picked?.let { p -> CommitPopup(p) { picked = null } }
}

/** The kinds of thing in the to-do list, each with its badge and its line in the filter. */
enum class TodoKind(val label: String) {
    BRANCH("Unmerged branches"),
    PR("Open PRs"),
    DRAFT("Draft PRs"),
    CHECKS("Failing checks"),
    UNREAD("Couldn't read"),
}

/** One thing to do in the to-do list, and where tapping it goes. */
private class Todo(
    val kind: TodoKind,
    val repo: Repo,
    val title: String,
    val detail: String,
    val color: Color,
    val url: String,
    /** When it last moved, ISO; empty when unknown, which sorts last. */
    val since: String,
)

private fun todos(snap: Snapshot, c: Colors): List<Todo> = buildList {
    for (repo in snap.repos) {
        repo.problem?.let { add(Todo(TodoKind.UNREAD, repo, "Couldn't read", it, c.amber, repo.url, "")) }
        if (repo.checksState == Checks.FAILING) {
            add(Todo(TodoKind.CHECKS, repo, "Checks failing on ${repo.defaultBranch}", "pushed ${ago(repo.pushedAt)}", c.red, "${repo.url}/actions", repo.pushedAt))
        }
        // Branches not merged with no PR; a branch with a PR is listed as its PR.
        for (b in repo.hanging.filter { it.pr == null }) {
            val parts = listOfNotNull(
                "+${b.notInMain} not in ${repo.defaultBranch}",
                "no PR",
                ago(b.committedAt).takeIf { it.isNotEmpty() },
            )
            add(Todo(TodoKind.BRANCH, repo, b.name, parts.joinToString(" · "), c.red, b.url, b.committedAt))
        }
        for (pr in repo.openPrs) {
            val branch = repo.branches.firstOrNull { it.name == pr.headBranch || it.pr?.number == pr.number }
            val since = branch?.committedAt.orEmpty()
            val parts = listOfNotNull(
                pr.headBranch,
                branch?.takeIf { it.hanging }?.let { "+${it.notInMain} not in ${repo.defaultBranch}" },
                ago(since).takeIf { it.isNotEmpty() },
            )
            add(
                Todo(
                    if (pr.draft) TodoKind.DRAFT else TodoKind.PR,
                    repo,
                    "#${pr.number} ${pr.title}",
                    parts.joinToString(" · "),
                    if (pr.draft) c.muted else c.green,
                    pr.url,
                    since,
                ),
            )
        }
    }
}.sortedWith(compareBy<Todo> { it.since.isEmpty() }.thenBy { it.since })

/**
 * Everything left to do across the repos, oldest first: branches not merged, open PRs,
 * failing checks and repos that couldn't be read. Tapping one opens it on GitHub; when the
 * repos were read and why a read failed is at the bottom.
 */
@Composable
private fun TodoSheet(snap: Snapshot, problem: String?) {
    val c = LocalColors.current
    val uri = LocalUriHandler.current
    val context = LocalContext.current
    val all = remember(snap, c) { todos(snap, c) }
    var hidden by remember { mutableStateOf(Store(context).todoHidden) }
    var filter by remember { mutableStateOf(false) }
    val items = all.filter { it.kind.name !in hidden }
    Column(Modifier.verticalScroll(rememberScrollState()).padding(start = 16.dp, end = 16.dp, bottom = 24.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                when (items.size) { 0 -> "Nothing to do"; 1 -> "1 thing to do"; else -> "${items.size} things to do" },
                color = if (items.isEmpty()) c.green else c.text,
                fontWeight = FontWeight.SemiBold,
                fontSize = 17.sp,
                modifier = Modifier.weight(1f),
            )
            // Tick the kinds to show; kept for next time.
            Box {
                TextButton(onClick = { filter = true }) {
                    Text(if (hidden.isEmpty()) "Filter" else "Filter · ${TodoKind.entries.size - hidden.size}/${TodoKind.entries.size}")
                }
                DropdownMenu(expanded = filter, onDismissRequest = { filter = false }) {
                    for (k in TodoKind.entries) {
                        val on = k.name !in hidden
                        DropdownMenuItem(
                            leadingIcon = { Checkbox(checked = on, onCheckedChange = null) },
                            text = {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    KindBadge(k, 16.dp)
                                    Spacer(Modifier.width(8.dp))
                                    Text("${k.label} (${all.count { it.kind == k }})")
                                }
                            },
                            onClick = {
                                hidden = if (on) hidden + k.name else hidden - k.name
                                Store(context).todoHidden = hidden
                            },
                        )
                    }
                }
            }
        }
        if (items.isEmpty()) {
            Text(
                if (all.isEmpty()) "Every branch is merged and no PR is open." else "Nothing of the kinds ticked in Filter.",
                color = c.muted,
                fontSize = 13.sp,
            )
        }
        Spacer(Modifier.height(6.dp))
        for (t in items) {
            Row(
                Modifier.fillMaxWidth().clickable { uri.openUri(t.url) }.padding(vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                // The repo's icon with a badge saying what kind of thing it is.
                Box(Modifier.size(26.dp)) {
                    RepoMark(t.repo, 22.dp)
                    KindBadge(t.kind, 13.dp, Modifier.align(Alignment.BottomEnd))
                }
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        t.title,
                        color = c.text,
                        fontFamily = if (t.title.startsWith("#")) FontFamily.Default else FontFamily.Monospace,
                        fontSize = 14.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        t.repo.name + " · " + t.detail,
                        color = t.color,
                        fontSize = 12.sp,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
        Spacer(Modifier.height(10.dp))
        Box(Modifier.fillMaxWidth().height(1.dp).background(c.border))
        Text("Read ${ago(snap.readAt)} · ${snap.repos.size} repos", color = c.muted, fontSize = 12.sp, modifier = Modifier.padding(top = 8.dp))
        problem?.let { Text(it, color = c.amber, fontSize = 13.sp, modifier = Modifier.padding(top = 4.dp)) }
    }
}

/**
 * A to-do kind's badge: a small disc in its colour with a white sign, a branch, a pull
 * request, a cross for failing checks, a "!" for a repo that couldn't be read.
 */
@Composable
fun KindBadge(kind: TodoKind, size: Dp, modifier: Modifier = Modifier) {
    val c = LocalColors.current
    val fill = when (kind) {
        TodoKind.BRANCH -> c.red
        TodoKind.PR -> c.green
        TodoKind.DRAFT -> c.muted
        TodoKind.CHECKS -> c.red
        TodoKind.UNREAD -> c.amber
    }
    Canvas(modifier.size(size)) {
        val d = this.size.minDimension
        drawCircle(c.surface, d / 2)
        drawCircle(fill, d / 2 - d * 0.08f)
        val w = Color.White
        val sw = d * 0.1f
        fun p(x: Float, y: Float) = Offset(x * d, y * d)
        when (kind) {
            TodoKind.BRANCH -> {
                drawLine(w, p(0.38f, 0.26f), p(0.38f, 0.74f), sw, StrokeCap.Round)
                drawPath(
                    Path().apply { moveTo(0.64f * d, 0.34f * d); cubicTo(0.64f * d, 0.55f * d, 0.38f * d, 0.5f * d, 0.38f * d, 0.66f * d) },
                    w,
                    style = Stroke(sw, cap = StrokeCap.Round),
                )
                drawCircle(w, d * 0.08f, p(0.64f, 0.3f))
            }
            TodoKind.PR, TodoKind.DRAFT -> {
                drawLine(w, p(0.34f, 0.28f), p(0.34f, 0.72f), sw, StrokeCap.Round)
                drawLine(w, p(0.66f, 0.46f), p(0.66f, 0.72f), sw, StrokeCap.Round)
                drawPath(
                    Path().apply { moveTo(0.66f * d, 0.46f * d); lineTo(0.66f * d, 0.32f * d); lineTo(0.5f * d, 0.32f * d) },
                    w,
                    style = Stroke(sw, cap = StrokeCap.Round),
                )
                drawLine(w, p(0.5f, 0.32f), p(0.57f, 0.25f), sw, StrokeCap.Round)
                drawLine(w, p(0.5f, 0.32f), p(0.57f, 0.39f), sw, StrokeCap.Round)
            }
            TodoKind.CHECKS -> {
                drawLine(w, p(0.33f, 0.33f), p(0.67f, 0.67f), sw * 1.2f, StrokeCap.Round)
                drawLine(w, p(0.67f, 0.33f), p(0.33f, 0.67f), sw * 1.2f, StrokeCap.Round)
            }
            TodoKind.UNREAD -> {
                drawLine(w, p(0.5f, 0.26f), p(0.5f, 0.56f), sw * 1.2f, StrokeCap.Round)
                drawCircle(w, d * 0.07f, p(0.5f, 0.72f))
            }
        }
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
