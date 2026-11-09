package app.portalinbox.alert

import app.portalinbox.proto.RequestKind
import app.portalinbox.ui.Ages

/**
 * The digest of several portals, as a part of the one notification (lot MP5, decision "one notification"). Pure Kotlin.
 *
 * A `digest` frame has numbers and an age, never a body. Each portal sends its own frame on its own schedule. The phone does not
 * post a notification for a frame. It keeps the last frame of each portal in a [Memory], turns it into a [Waiting] with
 * [merge], and [Attention.compute] adds one entry and one text for it to the notification that already exists.
 *
 * Rules:
 * - An id that the phone knows as closed leaves the count. The owner may have answered after the portal built the digest.
 * - An id that the phone does not know counts as waiting: the portal says it is open, and the phone may not have it yet.
 * - A portal with no waiting item has no digest part. When no portal has one, the notification has no digest part at all.
 * - A digest is a reminder, not a state. It stays in the text for [LIFETIME_MS], or until a newer frame of the same portal
 *   replaces it, or until nothing remains in it. The items stay in the counts of the notification.
 */
object DigestPlan {
    /** How long the phone keeps the text of a digest in the notification: 24 hours on the clock of the phone. */
    const val LIFETIME_MS = 24 * 3_600_000L

    /** The last `digest` frame of one portal. [receivedAtMs] is the time of the phone. [ids] is the oldest first. */
    data class Frame(
        val serverId: Long,
        val deliveryId: String,
        val count: Long,
        val oldestCreatedAtMs: Long,
        val ids: List<String>,
        val receivedAtMs: Long,
    )

    /** What the phone knows about one listed id. [createdAtMs] is on the clock of the portal. */
    data class Known(val kind: RequestKind, val open: Boolean, val createdAtMs: Long)

    /** What still waits at one portal, after the ids that the phone knows as closed left. [oldestCreatedAtMs] is on the clock of the portal. */
    data class Waiting(val serverId: Long, val deliveryId: String, val count: Long, val oldestCreatedAtMs: Long)

    /**
     * What still waits for [frame], or null when nothing does. [known] maps a request id to what the phone knows.
     *
     * The oldest time is the smallest time of the listed ids that are not closed, when the phone knows all of them. Else it is the
     * time of the frame. The frame lists the oldest items first, so the items that it does not list are not older than these.
     */
    fun waiting(frame: Frame, known: Map<String, Known>): Waiting? {
        val closed = frame.ids.count { known[it]?.open == false }
        val count = frame.count - closed
        if (count <= 0) return null
        val times = frame.ids.filter { known[it]?.open != false }.map { known[it]?.createdAtMs }
        val oldest = if (times.isNotEmpty() && times.all { it != null }) times.minOf { it!! } else frame.oldestCreatedAtMs
        return Waiting(frame.serverId, frame.deliveryId, count, oldest)
    }

    /**
     * The merged digest: one [Waiting] for each portal that has a current frame with something left. [known] gives what the phone
     * knows for a portal. The order is the order of [frames]. A frame older than [LIFETIME_MS] is left out.
     */
    fun merge(frames: Collection<Frame>, known: (Long) -> Map<String, Known>, phoneNowMs: Long): List<Waiting> =
        frames.filter { phoneNowMs - it.receivedAtMs < LIFETIME_MS }.mapNotNull { waiting(it, known(it.serverId)) }

    /** The text of the waiting part of a line: "3 waiting, oldest 6 days". [ageMs] is on the clock of the portal. */
    fun text(count: Long, ageMs: Long): String = "$count waiting, oldest " + Ages.long(ageMs)

    /**
     * The last frame of each portal. The notifier holds one. A newer frame of a portal replaces the older one. A frame with the
     * delivery id that the memory already holds is ignored: a delivery that comes again is not news.
     */
    class Memory {
        private val frames = LinkedHashMap<Long, Frame>()

        @Synchronized
        fun record(frame: Frame): Boolean {
            if (frames[frame.serverId]?.deliveryId == frame.deliveryId) return false
            frames.remove(frame.serverId)
            frames[frame.serverId] = frame
            return true
        }

        @Synchronized
        fun forget(serverId: Long) {
            frames.remove(serverId)
        }

        @Synchronized
        fun frames(): List<Frame> = frames.values.toList()
    }
}
