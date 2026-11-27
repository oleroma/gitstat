package gitstat.phone

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.glance.appwidget.updateAll
import gitstat.core.CommitDetail
import gitstat.core.GitHub
import gitstat.core.GitHubException
import gitstat.core.Repo
import gitstat.core.Snapshot
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/** What the screens show. */
data class UiState(
    val signedIn: Boolean = false,
    val snapshot: Snapshot? = null,
    val refreshing: Boolean = false,
    val problem: String? = null,
    val notify: Boolean = true,
    val rows: Boolean = false,
)

/**
 * The one place that reads GitHub, for the app, the widget and the background refresh alike:
 * one read at a time, saved to the [Store], then the widget is redrawn and newly red repos
 * notified.
 */
object Hub {
    private val _state = MutableStateFlow(UiState())
    val state: StateFlow<UiState> = _state.asStateFlow()
    private val lock = Mutex()
    @Volatile private var loaded = false

    fun load(context: Context) {
        if (loaded) return
        synchronized(this) {
            if (loaded) return
            val store = Store(context)
            _state.value = UiState(
                signedIn = store.token != null,
                snapshot = store.snapshot(),
                problem = store.problem,
                notify = store.notify,
                rows = store.rows,
            )
            loaded = true
        }
    }

    /** Reads GitHub now. Returns the problem, or null when it worked. */
    suspend fun refresh(context: Context): String? = lock.withLock {
        load(context)
        val store = Store(context)
        val token = store.token ?: return@withLock "Not signed in"
        _state.update { it.copy(refreshing = true) }
        try {
            val before = withContext(Dispatchers.IO) { store.snapshot() }
            val problem = try {
                val snap = withContext(Dispatchers.IO) { GitHub(GitHub.https(token)).read() }
                withContext(Dispatchers.IO) { store.save(snap) }
                _state.update { it.copy(snapshot = snap) }
                if (store.notify) notifyRed(context, snap.newlyRed(before))
                null
            } catch (e: GitHubException) {
                e.message ?: "GitHub couldn't be read"
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                "Couldn't read GitHub: ${e.message ?: e.javaClass.simpleName}"
            }
            store.problem = problem
            _state.update { it.copy(problem = problem) }
            runCatching { GitstatWidget().updateAll(context) }
            problem
        } finally {
            // Also when the screen that asked is closed mid-read.
            _state.update { it.copy(refreshing = false) }
        }
    }

    /** Saves [token] and reads with it; a token GitHub refuses isn't kept. */
    suspend fun signIn(context: Context, token: String): String? {
        load(context)
        val store = Store(context)
        store.token = token.trim()
        _state.update { it.copy(signedIn = true, problem = null) }
        val problem = refresh(context)
        if (problem != null && _state.value.snapshot == null) {
            store.forget()
            _state.update { it.copy(signedIn = false, problem = problem) }
        }
        return problem
    }

    suspend fun signOut(context: Context) {
        Store(context).forget()
        _state.value = UiState(notify = Store(context).notify)
        runCatching { GitstatWidget().updateAll(context) }
    }

    fun setNotify(context: Context, on: Boolean) {
        Store(context).notify = on
        _state.update { it.copy(notify = on) }
    }

    fun setRows(context: Context, on: Boolean) {
        Store(context).rows = on
        _state.update { it.copy(rows = on) }
    }

    /** A commit's whole message and size, for its card. */
    suspend fun commit(context: Context, repo: Repo, id: String): Result<CommitDetail> {
        val token = Store(context).token ?: return Result.failure(GitHubException("Not signed in"))
        return withContext(Dispatchers.IO) {
            runCatching { GitHub(GitHub.https(token)).commit(repo, id) }
        }
    }

    const val CHANNEL = "red"

    fun createChannel(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java) ?: return
        val channel = NotificationChannel(
            CHANNEL,
            context.getString(R.string.channel_red),
            NotificationManager.IMPORTANCE_DEFAULT,
        ).apply { description = context.getString(R.string.channel_red_description) }
        nm.createNotificationChannel(channel)
    }

    private fun notifyRed(context: Context, repos: List<Repo>) {
        if (repos.isEmpty()) return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        val open = PendingIntent.getActivity(
            context,
            0,
            Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val nm = NotificationManagerCompat.from(context)
        for (repo in repos) {
            val hanging = repo.hanging
            val text = hanging.joinToString(", ") { "${it.name} (+${it.notInMain})" }
            val n = NotificationCompat.Builder(context, CHANNEL)
                .setSmallIcon(R.drawable.ic_notify)
                .setContentTitle("${repo.name}: ${branchCount(hanging.size)} not merged")
                .setContentText(text)
                .setStyle(NotificationCompat.BigTextStyle().bigText(text))
                .setContentIntent(open)
                .setAutoCancel(true)
                .build()
            try {
                nm.notify(repo.slug.hashCode(), n)
            } catch (e: SecurityException) {
                return
            }
        }
    }
}

fun branchCount(n: Int) = if (n == 1) "1 branch" else "$n branches"
