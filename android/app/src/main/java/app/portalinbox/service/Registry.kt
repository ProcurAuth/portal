package app.portalinbox.service

import android.os.Handler
import android.os.Looper
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArraySet

/**
 * The state of the link to one portal. [REENROLL]: the portal revoked this device, or the address is invalid. [KEY_CHANGED]:
 * the portal does not prove the pinned key (see [KeyChange]). Both stop the connection until the user acts.
 */
enum class LinkState {
    CONNECTING, CONNECTED, DISCONNECTED, REENROLL, KEY_CHANGED;

    /** The phone tries to reach the portal, or failed: the two states count as one period without a link. */
    val notConnected: Boolean get() = this == CONNECTING || this == DISCONNECTED
}

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
    private val refusals = ConcurrentHashMap<Long, String>()
    private val listeners = CopyOnWriteArraySet<() -> Unit>()
    private val main = Handler(Looper.getMainLooper())

    fun status(serverId: Long): LinkStatus =
        statuses[serverId] ?: LinkStatus(LinkState.DISCONNECTED, System.currentTimeMillis(), null)

    fun setStatus(serverId: Long, state: LinkState, detail: String?) {
        val prev = statuses[serverId]
        // CONNECTING and DISCONNECTED are one period "not connected": each new attempt must not restart the clock, or a portal that
        // stays down would never reach its offline threshold (the home list and the warning use `sinceMs`).
        val sameSpell = prev != null && (prev.state == state || (prev.state.notConnected && state.notConnected))
        val since = if (prev != null && sameSpell) prev.sinceMs else System.currentTimeMillis()
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

    /** The text of the last `invalid_prefs` of this portal, or null. It lives as long as the process: the pending edit is already gone. */
    fun refusal(serverId: Long): String? = refusals[serverId]

    /** Records the refusal of a `set_prefs` (a text), or clears it (null). */
    fun setRefusal(serverId: Long, message: String?) {
        if (message == null) refusals.remove(serverId) else refusals[serverId] = message
        changed()
    }

    fun forget(serverId: Long) {
        refusals.remove(serverId)
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
