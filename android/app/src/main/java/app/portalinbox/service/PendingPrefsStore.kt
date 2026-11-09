package app.portalinbox.service

import android.content.Context
import app.portalinbox.data.PendingPrefs
import app.portalinbox.data.Store
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.StrictJson

/**
 * The edits of the preferences that the portal did not confirm yet ([app.portalinbox.data.PrefsSync]). They live in the column
 * `pending_prefs` of the row of the portal, one JSON text for each portal: removing the portal removes its edit. A text that does not
 * read back is dropped: the next `welcome` brings the values of the portal.
 */
class PendingPrefsStore private constructor(private val store: Store) : PendingPrefs {
    override fun get(serverId: Long): Prefs? {
        val text = store.pendingPrefs(serverId) ?: return null
        return runCatching { Prefs.fromJson(StrictJson.parseObject(text)) }.getOrNull()
    }

    override fun set(serverId: Long, prefs: Prefs) {
        store.setPendingPrefs(serverId, StrictJson.compact(prefs.toJson()))
    }

    override fun clear(serverId: Long) {
        store.setPendingPrefs(serverId, null)
    }

    companion object {
        @Volatile private var instance: PendingPrefsStore? = null

        fun get(context: Context): PendingPrefsStore =
            instance ?: synchronized(this) { instance ?: PendingPrefsStore(Store.get(context)).also { instance = it } }
    }
}
