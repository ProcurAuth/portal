package app.portalinbox.alert

import app.portalinbox.proto.RequestKind
import app.portalinbox.ui.Ages

/**
 * The text of the notification of a `digest` frame (DIAL 4.6). Pure Kotlin.
 *
 * The frame has numbers and an age, never a body. The phone adds only what it knows by itself: the kind of each listed item
 * (approval or question), taken from its own store, and whether the owner already closed some of them. The text never holds a
 * title, a command or an explanation: a digest can show on a locked screen.
 */
object DigestText {
    data class Content(val title: String, val text: String, val publicTitle: String, val publicText: String)

    /** What the phone knows about one listed id. */
    data class Known(val kind: RequestKind, val open: Boolean, val createdAtMs: Long)

    /**
     * The notification for a digest, or null when the phone knows that nothing waits any more (the owner answered after the
     * server built the digest). [known] maps a request id to what the phone knows. [nowMs] is the time of the server.
     */
    fun of(count: Long, oldestCreatedAtMs: Long, ids: List<String>, known: Map<String, Known>, nowMs: Long): Content? {
        // The list is complete when it names each open item, and the phone knows each of them.
        val complete = ids.size.toLong() == count && ids.all { it in known }
        val shown: Long
        val oldest: Long
        var kinds: List<RequestKind>? = null
        if (complete) {
            val open = ids.mapNotNull { known[it] }.filter { it.open }
            shown = open.size.toLong()
            oldest = open.minOfOrNull { it.createdAtMs } ?: oldestCreatedAtMs
            kinds = open.map { it.kind }
        } else {
            val closed = ids.count { known[it]?.open == false }
            shown = count - closed
            oldest = oldestCreatedAtMs
        }
        if (shown <= 0) return null
        val title = if (shown == 1L) "1 item waits for you" else "$shown items wait for you"
        val age = "The oldest is " + Ages.long((nowMs - oldest).coerceAtLeast(0)) + " old."
        val parts = kinds?.let { breakdown(it) }
        return Content(
            title = title,
            text = if (parts == null) age else "$parts. $age",
            publicTitle = title,
            publicText = age,
        )
    }

    /** "1 approval and 1 question", "2 approvals", "1 approval, 2 questions". Null when there is no kind. */
    fun breakdown(kinds: List<RequestKind>): String? {
        val approvals = kinds.count { it == RequestKind.APPROVAL }
        val questions = kinds.count { it == RequestKind.QUESTION }
        val parts = buildList {
            if (approvals > 0) add(if (approvals == 1) "1 approval" else "$approvals approvals")
            if (questions > 0) add(if (questions == 1) "1 question" else "$questions questions")
        }
        return when (parts.size) {
            0 -> null
            1 -> parts[0]
            else -> parts[0] + " and " + parts[1]
        }
    }
}
