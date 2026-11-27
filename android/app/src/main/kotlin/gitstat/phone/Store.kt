package gitstat.phone

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.edit
import gitstat.core.Snapshot
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * What the app keeps on the phone: the GitHub token (encrypted with a key that never leaves
 * the Android Keystore), a few settings, and the last snapshot so the app and widget show
 * something at once. Backups are off in the manifest, so none of it leaves the phone.
 */
class Store(context: Context) {
    private val app = context.applicationContext
    private val prefs = app.getSharedPreferences("gitstat", Context.MODE_PRIVATE)
    private val snapshotFile = File(app.filesDir, "snapshot.json")

    var token: String?
        get() = prefs.getString(KEY_TOKEN, null)?.let { Vault.open(it) }
        set(value) {
            prefs.edit {
                if (value == null) remove(KEY_TOKEN) else putString(KEY_TOKEN, Vault.seal(value))
            }
        }

    /** Notify when a repo turns red. */
    var notify: Boolean
        get() = prefs.getBoolean(KEY_NOTIFY, true)
        set(value) = prefs.edit { putBoolean(KEY_NOTIFY, value) }

    /** The horizontal view (a row per repo) instead of one repo per screen. */
    var rows: Boolean
        get() = prefs.getBoolean(KEY_ROWS, false)
        set(value) = prefs.edit { putBoolean(KEY_ROWS, value) }

    /** Why the last refresh failed, or null when it worked. */
    var problem: String?
        get() = prefs.getString(KEY_PROBLEM, null)
        set(value) = prefs.edit { putString(KEY_PROBLEM, value) }

    fun snapshot(): Snapshot? = try {
        if (snapshotFile.exists()) Snapshot.fromJson(snapshotFile.readText()) else null
    } catch (e: Exception) {
        null
    }

    fun save(snapshot: Snapshot) {
        val tmp = File(app.filesDir, "snapshot.json.tmp")
        tmp.writeText(snapshot.toJson())
        tmp.renameTo(snapshotFile)
    }

    fun forget() {
        token = null
        problem = null
        snapshotFile.delete()
    }

    private companion object {
        const val KEY_TOKEN = "token"
        const val KEY_NOTIFY = "notify"
        const val KEY_PROBLEM = "problem"
        const val KEY_ROWS = "rows"
    }
}

/** AES-GCM with a key kept in the Android Keystore. */
private object Vault {
    private const val ALIAS = "gitstat-token"
    private const val TRANSFORM = "AES/GCM/NoPadding"

    private fun key(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .build(),
        )
        return gen.generateKey()
    }

    fun seal(text: String): String {
        val c = Cipher.getInstance(TRANSFORM)
        c.init(Cipher.ENCRYPT_MODE, key())
        val sealed = c.doFinal(text.toByteArray())
        return b64(c.iv) + ":" + b64(sealed)
    }

    /** The token, or null when it can't be opened (such as after the key was reset). */
    fun open(stored: String): String? = try {
        val (iv, sealed) = stored.split(':', limit = 2)
        val c = Cipher.getInstance(TRANSFORM)
        c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, unb64(iv)))
        String(c.doFinal(unb64(sealed)))
    } catch (e: Exception) {
        null
    }

    private fun b64(b: ByteArray) = Base64.encodeToString(b, Base64.NO_WRAP)
    private fun unb64(s: String) = Base64.decode(s, Base64.NO_WRAP)
}
