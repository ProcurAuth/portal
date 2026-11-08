package app.portalinbox.testsupport

import app.portalinbox.data.Item
import app.portalinbox.data.ItemRepository
import app.portalinbox.data.OutboxEntry
import app.portalinbox.data.StoredPrefs
import app.portalinbox.data.Turn
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.SettledFrame

/** The storage of the engine in memory. A transaction that throws puts every table back as it was. */
class MemoryItemRepository : ItemRepository {
    private data class Key(val serverId: Long, val id: String)

    private var items = LinkedHashMap<Key, Item>()
    private var deliveries = HashMap<Key, Long>()
    private var turns = ArrayList<Turn>()
    private var early = HashMap<Key, SettledFrame>()
    private var outbox = LinkedHashMap<Key, OutboxEntry>()
    private var prefs = HashMap<Long, StoredPrefs>()

    var transactions = 0
        private set

    @Suppress("UNCHECKED_CAST")
    override fun <T> transaction(block: () -> T): T {
        transactions++
        val saved = listOf(LinkedHashMap(items), HashMap(deliveries), ArrayList(turns), HashMap(early), LinkedHashMap(outbox), HashMap(prefs))
        try {
            return block()
        } catch (e: Throwable) {
            items = saved[0] as LinkedHashMap<Key, Item>
            deliveries = saved[1] as HashMap<Key, Long>
            turns = saved[2] as ArrayList<Turn>
            early = saved[3] as HashMap<Key, SettledFrame>
            outbox = saved[4] as LinkedHashMap<Key, OutboxEntry>
            prefs = saved[5] as HashMap<Long, StoredPrefs>
            throw e
        }
    }

    override fun item(serverId: Long, requestId: String): Item? = items[Key(serverId, requestId)]

    override fun putItem(item: Item) {
        items[Key(item.serverId, item.requestId)] = item
    }

    override fun markNotified(serverId: Long, requestId: String) {
        items[Key(serverId, requestId)]?.let { items[Key(serverId, requestId)] = it.copy(notified = true) }
    }

    override fun openItems(serverId: Long): List<Item> =
        items.values.filter { it.serverId == serverId && it.state.isOpen }.sortedWith(compareBy({ it.request.createdAtMs }, { it.requestId }))

    override fun items(serverId: Long): List<Item> =
        items.values.filter { it.serverId == serverId }.sortedWith(compareByDescending<Item> { it.request.createdAtMs }.thenBy { it.requestId })

    override fun hasDelivery(serverId: Long, deliveryId: String) = Key(serverId, deliveryId) in deliveries

    override fun addDelivery(serverId: Long, deliveryId: String, atMs: Long) {
        deliveries[Key(serverId, deliveryId)] = atMs
    }

    override fun putTurn(turn: Turn): Boolean {
        if (turns.any { it.serverId == turn.serverId && it.requestId == turn.requestId && it.seq == turn.seq }) return false
        turns += turn
        return true
    }

    override fun turns(serverId: Long, requestId: String): List<Turn> =
        turns.filter { it.serverId == serverId && it.requestId == requestId }.sortedBy { it.seq }

    override fun putEarlySettlement(serverId: Long, frame: SettledFrame) {
        early[Key(serverId, frame.requestId)] = frame
    }

    override fun takeEarlySettlement(serverId: Long, requestId: String): SettledFrame? = early.remove(Key(serverId, requestId))

    override fun putOutbox(entry: OutboxEntry) {
        outbox[Key(entry.serverId, entry.clientMsgId)] = entry
    }

    override fun outbox(serverId: Long, clientMsgId: String): OutboxEntry? = outbox[Key(serverId, clientMsgId)]

    override fun pendingOutbox(serverId: Long): List<OutboxEntry> =
        outbox.values.filter { it.serverId == serverId && it.isPending }.sortedWith(compareBy({ it.createdAtMs }, { it.clientMsgId }))

    override fun outboxOf(serverId: Long, requestId: String): List<OutboxEntry> =
        outbox.values.filter { it.serverId == serverId && it.requestId == requestId }.sortedWith(compareBy({ it.createdAtMs }, { it.clientMsgId }))

    override fun markSent(serverId: Long, clientMsgId: String, atMs: Long) {
        val k = Key(serverId, clientMsgId)
        outbox[k]?.let { outbox[k] = it.copy(attempts = it.attempts + 1, lastSentAtMs = atMs) }
    }

    override fun closeOutbox(serverId: Long, clientMsgId: String, result: String, atMs: Long) {
        val k = Key(serverId, clientMsgId)
        outbox[k]?.takeIf { it.isPending }?.let { outbox[k] = it.copy(result = result, closedAtMs = atMs) }
    }

    override fun closeOutboxOf(serverId: Long, requestId: String, result: String, atMs: Long) {
        for ((k, e) in outbox.entries.toList()) {
            if (e.serverId == serverId && e.requestId == requestId && e.isPending) outbox[k] = e.copy(result = result, closedAtMs = atMs)
        }
    }

    override fun deleteItem(serverId: Long, requestId: String) {
        items.remove(Key(serverId, requestId))
        turns.removeAll { it.serverId == serverId && it.requestId == requestId }
        early.remove(Key(serverId, requestId))
        outbox.entries.removeAll { it.value.serverId == serverId && it.value.requestId == requestId }
    }

    override fun deleteDeliveriesBefore(serverId: Long, beforeMs: Long) {
        deliveries.entries.removeAll { it.key.serverId == serverId && it.value < beforeMs }
    }

    fun deliveryCount(serverId: Long): Int = deliveries.keys.count { it.serverId == serverId }

    override fun prefs(serverId: Long): StoredPrefs? = prefs[serverId]

    override fun putPrefs(serverId: Long, prefs: StoredPrefs, atMs: Long) {
        this.prefs[serverId] = prefs
    }

    fun states(serverId: Long): Map<String, ItemState> = items.values.filter { it.serverId == serverId }.associate { it.requestId to it.state }
}
