package app.portalinbox.service

import android.content.Context
import app.portalinbox.data.PendingPrefs
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.StrictJson

/**
 * The edits of the preferences that the server did not confirm yet ([app.portalinbox.data.PrefsSync]). One small file of the
 * app, one JSON text for each server. A text that does not read back is dropped: the next `welcome` brings the values of the server.
 */
class PendingPrefsStore private constructor(context: Context) : PendingPrefs {
    private val file = context.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    private fun key(serverId: Long) = "server:$serverId"

    override fun get(serverId: Long): Prefs? {
        val text = file.getString(key(serverId), null) ?: return null
        return runCatching { Prefs.fromJson(StrictJson.parseObject(text)) }.getOrNull()
    }

    override fun set(serverId: Long, prefs: Prefs) {
        file.edit().putString(key(serverId), StrictJson.compact(prefs.toJson())).apply()
    }

    override fun clear(serverId: Long) {
        file.edit().remove(key(serverId)).apply()
    }

    companion object {
        private const val FILE = "inbox_prefs_pending"

        @Volatile private var instance: PendingPrefsStore? = null

        fun get(context: Context): PendingPrefsStore =
            instance ?: synchronized(this) { instance ?: PendingPrefsStore(context).also { instance = it } }
    }
}
