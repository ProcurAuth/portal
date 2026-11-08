package app.portalinbox.data

import app.portalinbox.proto.SettledFrame

/**
 * The storage that the [ItemEngine] needs. The SQLite implementation is [SqlItemRepository].
 * Tests use a memory implementation. Each method of the engine that writes runs inside
 * [transaction], so a frame is stored whole or not at all before the receipt goes out.
 */
interface ItemRepository {
    fun <T> transaction(block: () -> T): T

    fun item(serverId: Long, requestId: String): Item?
    fun putItem(item: Item)
    fun markNotified(serverId: Long, requestId: String)

    /** Items of the server in state `pending` or `replied`, the oldest first. */
    fun openItems(serverId: Long): List<Item>

    /** Every item of the server, the most recent first. */
    fun items(serverId: Long): List<Item>

    /** True when the delivery was stored. The server sends a row again until the receipt arrives. */
    fun hasDelivery(serverId: Long, deliveryId: String): Boolean
    fun addDelivery(serverId: Long, deliveryId: String, atMs: Long)

    /** Stores the turn. False when the turn is already there. */
    fun putTurn(turn: Turn): Boolean
    fun turns(serverId: Long, requestId: String): List<Turn>

    /** A `settled` frame that came before its `request`. */
    fun putEarlySettlement(serverId: Long, frame: SettledFrame)
    fun takeEarlySettlement(serverId: Long, requestId: String): SettledFrame?

    fun putOutbox(entry: OutboxEntry)
    fun outbox(serverId: Long, clientMsgId: String): OutboxEntry?

    /** Entries not closed, the oldest first. */
    fun pendingOutbox(serverId: Long): List<OutboxEntry>

    /** Every entry of an item, the oldest first. */
    fun outboxOf(serverId: Long, requestId: String): List<OutboxEntry>
    fun markSent(serverId: Long, clientMsgId: String, atMs: Long)
    fun closeOutbox(serverId: Long, clientMsgId: String, result: String, atMs: Long)
    fun closeOutboxOf(serverId: Long, requestId: String, result: String, atMs: Long)

    fun prefs(serverId: Long): StoredPrefs?
    fun putPrefs(serverId: Long, prefs: StoredPrefs, atMs: Long)
}
