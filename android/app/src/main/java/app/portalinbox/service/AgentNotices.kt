package app.portalinbox.service

import java.util.concurrent.ConcurrentLinkedQueue

/**
 * The lines for the snackbar after an `agent_result`. The connection posts a line. The screen that is open takes
 * it and shows it once. A line that no screen takes in [MAX_KEPT] places is dropped: an old result never shows later.
 * Pure Kotlin: the queue holds only text of the app, never a text of an agent.
 */
object AgentNotices {
    const val MAX_KEPT = 8

    private val lines = ConcurrentLinkedQueue<String>()

    fun post(text: String) {
        lines.add(text)
        while (lines.size > MAX_KEPT) lines.poll()
    }

    /** The oldest line that waits, or null. The line leaves the queue. */
    fun take(): String? = lines.poll()

    fun clear() = lines.clear()
}
