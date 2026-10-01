package app.inbox.service

import android.os.Handler
import android.os.Looper
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArraySet

enum class LinkState { CONNECTING, CONNECTED, DISCONNECTED, REENROLL }

data class LinkStatus(val state: LinkState, val sinceMs: Long, val detail: String?)

/**
 * In-process registry shared by the service and the activities: live connections,
 * their status, and a change notification for the UI.
 */
object Registry {
    val connections = ConcurrentHashMap<Long, ServerConnection>()
    private val statuses = ConcurrentHashMap<Long, LinkStatus>()
    private val listeners = CopyOnWriteArraySet<() -> Unit>()
    private val main = Handler(Looper.getMainLooper())

    fun status(serverId: Long): LinkStatus =
        statuses[serverId] ?: LinkStatus(LinkState.DISCONNECTED, System.currentTimeMillis(), null)

    fun setStatus(serverId: Long, state: LinkState, detail: String?) {
        val prev = statuses[serverId]
        val since = if (prev != null && prev.state == state) prev.sinceMs else System.currentTimeMillis()
        statuses[serverId] = LinkStatus(state, since, detail)
        changed()
    }

    fun forget(serverId: Long) {
        statuses.remove(serverId)
        changed()
    }

    fun addListener(l: () -> Unit) = listeners.add(l)
    fun removeListener(l: () -> Unit) = listeners.remove(l)

    fun changed() {
        main.post { listeners.forEach { it() } }
    }
}
