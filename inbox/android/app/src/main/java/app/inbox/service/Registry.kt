package app.inbox.service

import android.os.Handler
import android.os.Looper
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArraySet

enum class LinkState { CONNECTING, CONNECTED, DISCONNECTED, REENROLL }

data class LinkStatus(val state: LinkState, val sinceMs: Long, val detail: String?)

/** Liveness of a session: last heartbeat answer (and its round trip) and last frame. */
data class Liveness(val lastPongMs: Long?, val rttMs: Long?, val lastFrameMs: Long?)

/**
 * In-process registry shared by the service and the activities: live connections,
 * their status, and a change notification for the UI.
 */
object Registry {
    val connections = ConcurrentHashMap<Long, ServerConnection>()
    private val statuses = ConcurrentHashMap<Long, LinkStatus>()
    private val liveness = ConcurrentHashMap<Long, Liveness>()
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

    /** Read by screens that tick on their own; updating it does not re-render anything. */
    fun liveness(serverId: Long): Liveness = liveness[serverId] ?: Liveness(null, null, null)

    fun frame(serverId: Long, atMs: Long) {
        liveness.compute(serverId) { _, l -> (l ?: Liveness(null, null, null)).copy(lastFrameMs = atMs) }
    }

    fun pong(serverId: Long, atMs: Long, rttMs: Long?) {
        liveness.compute(serverId) { _, l ->
            (l ?: Liveness(null, null, null)).copy(lastPongMs = atMs, rttMs = rttMs ?: l?.rttMs, lastFrameMs = atMs)
        }
    }

    fun forget(serverId: Long) {
        statuses.remove(serverId)
        liveness.remove(serverId)
        changed()
    }

    fun addListener(l: () -> Unit) = listeners.add(l)
    fun removeListener(l: () -> Unit) = listeners.remove(l)

    fun changed() {
        main.post { listeners.forEach { it() } }
    }
}
