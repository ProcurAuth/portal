package app.portalinbox.portal

import app.portalinbox.data.Item

/**
 * The order of the items of the home list when they come from several portals (proposal 3.2). Pure Kotlin.
 *
 * Each portal has its own clock, and a portal chooses the times that it sends. The raw `created_at_ms` is not a safe sort key:
 * a rogue portal could send 0 and put its items on top. The sort key is the creation time moved to the phone clock and then
 * limited to the 7 days before the item arrived on the phone. So a portal cannot claim an age of more than 7 days more than
 * the truth, and it can never claim a time after the arrival.
 *
 * The offset is `server time - phone time` (see `ClockOffsets`). A server time becomes a phone time with `time - offset`. The
 * proposal writes `+ clockOffset`: it uses the opposite sign for the same offset.
 */
object ItemSort {
    /** A portal cannot make an item look older than this, compared with the moment when the phone received it. */
    const val MAX_CLAIMED_AGE_MS = 7L * 24 * 3_600_000

    /**
     * The sort key of an item, on the phone clock. [serverTimeMs] is the time that the portal claims, [offsetMs] the clock
     * offset of this portal (0 when unknown), [receivedAtMs] the phone time of the arrival.
     */
    fun key(serverTimeMs: Long, offsetMs: Long, receivedAtMs: Long): Long =
        saturatingSub(serverTimeMs, offsetMs).coerceIn(saturatingSub(receivedAtMs, MAX_CLAIMED_AGE_MS), receivedAtMs)

    /** The sort key of the creation of an open item. */
    fun createdKey(item: Item, offsets: Map<Long, Long>): Long =
        key(item.request.createdAtMs, offsets[item.serverId] ?: 0L, item.receivedAtMs)

    /** The sort key of the end of a final item. The arrival of the `settled` frame (`updatedAtMs`) is the limit. */
    fun doneKey(item: Item, offsets: Map<Long, Long>): Long =
        item.settled?.let { key(it.atMs, offsets[item.serverId] ?: 0L, item.updatedAtMs) } ?: item.updatedAtMs

    /**
     * The open items, the oldest first, across portals. A tie goes to the server, then to the request id: the order never
     * jumps. [offsets] maps a server id to its clock offset.
     */
    fun toAnswer(items: List<Item>, offsets: Map<Long, Long>): List<Item> =
        items.filter { it.isOpen }
            .sortedWith(compareBy<Item> { createdKey(it, offsets) }.thenBy { it.serverId }.thenBy { it.requestId })

    /** The final items, the most recent end first, across portals. */
    fun done(items: List<Item>, offsets: Map<Long, Long>): List<Item> =
        items.filter { !it.isOpen }
            .sortedWith(compareByDescending<Item> { doneKey(it, offsets) }.thenBy { it.serverId }.thenBy { it.requestId })

    private fun saturatingSub(a: Long, b: Long): Long {
        val r = a - b
        return if (((a xor b) and (a xor r)) < 0) (if (a < 0) Long.MIN_VALUE else Long.MAX_VALUE) else r
    }
}
