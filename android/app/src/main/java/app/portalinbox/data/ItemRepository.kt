package app.portalinbox.data

import app.portalinbox.proto.SettledFrame

/** How many items of one portal wait: approvals and questions in state `pending` or `replied`. */
data class OpenCount(val approvals: Int, val questions: Int) {
    val total: Int get() = approvals + questions
}

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

    /**
     * The open items of every portal, counted by kind, in one pass (the filter chips of the home list count without
     * loading each item). A portal with no open item has no entry.
     */
    fun openCounts(): Map<Long, OpenCount>

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

    /** Erases an item and what belongs to it: its turns, its outbox rows and an early settlement. This is the local retention. */
    fun deleteItem(serverId: Long, requestId: String)

    /** Erases the closed outbox entries of the actions on agents that closed before [beforeMs]. An action has no item that a retention could erase. */
    fun deleteClosedAgentActionsBefore(serverId: Long, beforeMs: Long)

    /** Forgets the deliveries that arrived before [beforeMs]. The server stopped sending these rows long ago. */
    fun deleteDeliveriesBefore(serverId: Long, beforeMs: Long)

    /** The agent of a portal, or null. The key is the portal and the agent id: two portals never share an agent. */
    fun agent(serverId: Long, agentId: String): Agent?
    fun putAgent(agent: Agent)

    /** The agents of one portal. */
    fun agents(serverId: Long): List<Agent>

    /** The agents of every portal, for the screens that show several portals. */
    fun allAgents(): List<Agent>
    fun markAgentNotified(serverId: Long, agentId: String)

    fun prefs(serverId: Long): StoredPrefs?
    fun putPrefs(serverId: Long, prefs: StoredPrefs, atMs: Long)
}
