package app.portalinbox.data

import android.content.ContentValues
import android.database.Cursor
import android.database.sqlite.SQLiteDatabase
import app.portalinbox.core.ResolvedField
import app.portalinbox.core.ResolvedView
import app.portalinbox.proto.AgentFrame
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.MessageFrame
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.RequestFrame
import app.portalinbox.proto.RequestKind
import app.portalinbox.proto.ServerFrames
import app.portalinbox.proto.SettledFrame
import app.portalinbox.proto.StrictJson
import org.json.JSONArray
import org.json.JSONObject

/** The tables of the items. The upgrade of the database drops them and makes them again: no migration. */
object ItemSchema {
    val TABLES = listOf("items", "item_turns", "item_settled_early", "deliveries", "outbox", "server_prefs", "agents")

    fun drop(db: SQLiteDatabase) {
        for (t in TABLES) db.execSQL("DROP TABLE IF EXISTS $t")
    }

    fun create(db: SQLiteDatabase) {
        // `frame` is the request frame as JSON: the local copy of the body, kept for the history.
        db.execSQL(
            """CREATE TABLE items (
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                request_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                chain_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                created_at_ms INTEGER NOT NULL,
                frame TEXT NOT NULL,
                state TEXT NOT NULL,
                validity TEXT NOT NULL,
                computed_effect TEXT,
                view TEXT,
                settled TEXT,
                received_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                notified INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (server_id, request_id))"""
        )
        db.execSQL("CREATE INDEX items_state ON items(server_id, state, created_at_ms)")
        db.execSQL(
            """CREATE TABLE item_turns (
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                request_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                frame TEXT NOT NULL,
                received_at_ms INTEGER NOT NULL,
                PRIMARY KEY (server_id, request_id, seq))"""
        )
        db.execSQL(
            """CREATE TABLE item_settled_early (
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                request_id TEXT NOT NULL,
                frame TEXT NOT NULL,
                PRIMARY KEY (server_id, request_id))"""
        )
        db.execSQL(
            """CREATE TABLE deliveries (
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                delivery_id TEXT NOT NULL,
                received_at_ms INTEGER NOT NULL,
                PRIMARY KEY (server_id, delivery_id))"""
        )
        db.execSQL(
            """CREATE TABLE outbox (
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                client_msg_id TEXT NOT NULL,
                request_id TEXT NOT NULL,
                type TEXT NOT NULL,
                frame TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                last_sent_at_ms INTEGER,
                result TEXT,
                closed_at_ms INTEGER,
                PRIMARY KEY (server_id, client_msg_id))"""
        )
        db.execSQL("CREATE INDEX outbox_request ON outbox(server_id, request_id)")
        // One row for each portal and agent. `frame` is the last `agent` frame with the highest `rev`.
        db.execSQL(
            """CREATE TABLE agents (
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                agent_id TEXT NOT NULL,
                rev INTEGER NOT NULL,
                frame TEXT NOT NULL,
                received_at_ms INTEGER NOT NULL,
                notified INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (server_id, agent_id))"""
        )
        db.execSQL(
            """CREATE TABLE server_prefs (
                server_id INTEGER PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
                prefs TEXT NOT NULL,
                body_purge_after_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL)"""
        )
    }
}

/** The SQLite storage of the items. The rules are in [ItemEngine]. */
class SqlItemRepository(private val store: Store) : ItemRepository {
    private val db: SQLiteDatabase get() = store.writableDatabase

    override fun <T> transaction(block: () -> T): T {
        db.beginTransaction()
        try {
            val r = block()
            db.setTransactionSuccessful()
            return r
        } finally {
            db.endTransaction()
        }
    }

    // ------------------------------------------------------------ items

    private val itemColumns =
        "server_id, frame, state, validity, computed_effect, view, settled, received_at_ms, updated_at_ms, notified"

    private fun readItem(c: Cursor): Item {
        val request = ServerFrames.decode(c.getString(1)) as RequestFrame
        return Item(
            serverId = c.getLong(0),
            request = request,
            state = ItemState.of(c.getString(2)) ?: ItemState.CANCELLED,
            validity = Validity.of(c.getString(3)),
            computedEffect = if (c.isNull(4)) null else c.getString(4),
            view = if (c.isNull(5)) null else viewFromJson(c.getString(5)),
            settled = if (c.isNull(6)) null else ServerFrames.decode(c.getString(6)) as SettledFrame,
            receivedAtMs = c.getLong(7),
            updatedAtMs = c.getLong(8),
            notified = c.getInt(9) != 0,
        )
    }

    private fun queryItems(sql: String, args: Array<String>): List<Item> =
        db.rawQuery(sql, args).use { c -> buildList { while (c.moveToNext()) add(readItem(c)) } }

    override fun item(serverId: Long, requestId: String): Item? =
        queryItems("SELECT $itemColumns FROM items WHERE server_id = ? AND request_id = ?", arrayOf(serverId.toString(), requestId)).firstOrNull()

    override fun putItem(item: Item) {
        val v = ContentValues().apply {
            put("server_id", item.serverId)
            put("request_id", item.requestId)
            put("kind", item.kind.wire)
            put("chain_id", item.request.chainId)
            put("seq", item.request.seq)
            put("created_at_ms", item.request.createdAtMs)
            put("frame", StrictJson.compact(item.request.toJson()))
            put("state", item.state.wire)
            put("validity", item.validity.wire)
            put("computed_effect", item.computedEffect)
            put("view", item.view?.let { viewToJson(it) })
            put("settled", item.settled?.let { StrictJson.compact(it.toJson()) })
            put("received_at_ms", item.receivedAtMs)
            put("updated_at_ms", item.updatedAtMs)
            put("notified", if (item.notified) 1 else 0)
        }
        db.insertWithOnConflict("items", null, v, SQLiteDatabase.CONFLICT_REPLACE)
    }

    override fun markNotified(serverId: Long, requestId: String) {
        db.update(
            "items", ContentValues().apply { put("notified", 1) },
            "server_id = ? AND request_id = ?", arrayOf(serverId.toString(), requestId),
        )
    }

    override fun openItems(serverId: Long): List<Item> = queryItems(
        "SELECT $itemColumns FROM items WHERE server_id = ? AND state IN ('pending', 'replied') " +
            "ORDER BY created_at_ms, request_id",
        arrayOf(serverId.toString()),
    )

    override fun items(serverId: Long): List<Item> = queryItems(
        "SELECT $itemColumns FROM items WHERE server_id = ? ORDER BY created_at_ms DESC, request_id",
        arrayOf(serverId.toString()),
    )

    override fun openCounts(): Map<Long, OpenCount> {
        val approvals = HashMap<Long, Int>()
        val questions = HashMap<Long, Int>()
        db.rawQuery(
            "SELECT server_id, kind, COUNT(*) FROM items WHERE state IN ('pending', 'replied') GROUP BY server_id, kind", null,
        ).use { c ->
            while (c.moveToNext()) {
                val into = if (c.getString(1) == RequestKind.APPROVAL.wire) approvals else questions
                into[c.getLong(0)] = (into[c.getLong(0)] ?: 0) + c.getInt(2)
            }
        }
        return (approvals.keys + questions.keys).associateWith { OpenCount(approvals[it] ?: 0, questions[it] ?: 0) }
    }

    // ------------------------------------------------------------ deliveries

    override fun hasDelivery(serverId: Long, deliveryId: String): Boolean = db.rawQuery(
        "SELECT 1 FROM deliveries WHERE server_id = ? AND delivery_id = ?", arrayOf(serverId.toString(), deliveryId),
    ).use { it.moveToFirst() }

    override fun addDelivery(serverId: Long, deliveryId: String, atMs: Long) {
        db.insertWithOnConflict(
            "deliveries", null,
            ContentValues().apply { put("server_id", serverId); put("delivery_id", deliveryId); put("received_at_ms", atMs) },
            SQLiteDatabase.CONFLICT_IGNORE,
        )
    }

    // ------------------------------------------------------------ turns

    override fun putTurn(turn: Turn): Boolean = db.insertWithOnConflict(
        "item_turns", null,
        ContentValues().apply {
            put("server_id", turn.serverId)
            put("request_id", turn.requestId)
            put("seq", turn.seq)
            put("frame", StrictJson.compact(turn.frame.toJson()))
            put("received_at_ms", turn.receivedAtMs)
        },
        SQLiteDatabase.CONFLICT_IGNORE,
    ) != -1L

    override fun turns(serverId: Long, requestId: String): List<Turn> = db.rawQuery(
        "SELECT frame, received_at_ms FROM item_turns WHERE server_id = ? AND request_id = ? ORDER BY seq",
        arrayOf(serverId.toString(), requestId),
    ).use { c ->
        buildList { while (c.moveToNext()) add(Turn(serverId, ServerFrames.decode(c.getString(0)) as MessageFrame, c.getLong(1))) }
    }

    // ------------------------------------------------------------ early settlements

    override fun putEarlySettlement(serverId: Long, frame: SettledFrame) {
        db.insertWithOnConflict(
            "item_settled_early", null,
            ContentValues().apply {
                put("server_id", serverId); put("request_id", frame.requestId); put("frame", StrictJson.compact(frame.toJson()))
            },
            SQLiteDatabase.CONFLICT_REPLACE,
        )
    }

    override fun takeEarlySettlement(serverId: Long, requestId: String): SettledFrame? {
        val args = arrayOf(serverId.toString(), requestId)
        val frame = db.rawQuery("SELECT frame FROM item_settled_early WHERE server_id = ? AND request_id = ?", args)
            .use { c -> if (c.moveToFirst()) ServerFrames.decode(c.getString(0)) as SettledFrame else null }
        if (frame != null) db.delete("item_settled_early", "server_id = ? AND request_id = ?", args)
        return frame
    }

    // ------------------------------------------------------------ outbox

    private val outboxColumns =
        "server_id, client_msg_id, request_id, type, frame, created_at_ms, attempts, last_sent_at_ms, result, closed_at_ms"

    private fun readOutbox(c: Cursor) = OutboxEntry(
        serverId = c.getLong(0),
        clientMsgId = c.getString(1),
        requestId = c.getString(2),
        type = OutboxType.of(c.getString(3)),
        frame = c.getString(4),
        createdAtMs = c.getLong(5),
        attempts = c.getInt(6),
        lastSentAtMs = if (c.isNull(7)) null else c.getLong(7),
        result = if (c.isNull(8)) null else c.getString(8),
        closedAtMs = if (c.isNull(9)) null else c.getLong(9),
    )

    private fun queryOutbox(sql: String, args: Array<String>): List<OutboxEntry> =
        db.rawQuery(sql, args).use { c -> buildList { while (c.moveToNext()) add(readOutbox(c)) } }

    override fun putOutbox(entry: OutboxEntry) {
        db.insertWithOnConflict(
            "outbox", null,
            ContentValues().apply {
                put("server_id", entry.serverId)
                put("client_msg_id", entry.clientMsgId)
                put("request_id", entry.requestId)
                put("type", entry.type.wire)
                put("frame", entry.frame)
                put("created_at_ms", entry.createdAtMs)
                put("attempts", entry.attempts)
                put("last_sent_at_ms", entry.lastSentAtMs)
                put("result", entry.result)
                put("closed_at_ms", entry.closedAtMs)
            },
            SQLiteDatabase.CONFLICT_REPLACE,
        )
    }

    override fun outbox(serverId: Long, clientMsgId: String): OutboxEntry? = queryOutbox(
        "SELECT $outboxColumns FROM outbox WHERE server_id = ? AND client_msg_id = ?", arrayOf(serverId.toString(), clientMsgId),
    ).firstOrNull()

    override fun pendingOutbox(serverId: Long): List<OutboxEntry> = queryOutbox(
        "SELECT $outboxColumns FROM outbox WHERE server_id = ? AND closed_at_ms IS NULL ORDER BY created_at_ms, client_msg_id",
        arrayOf(serverId.toString()),
    )

    override fun outboxOf(serverId: Long, requestId: String): List<OutboxEntry> = queryOutbox(
        "SELECT $outboxColumns FROM outbox WHERE server_id = ? AND request_id = ? ORDER BY created_at_ms, client_msg_id",
        arrayOf(serverId.toString(), requestId),
    )

    override fun markSent(serverId: Long, clientMsgId: String, atMs: Long) {
        db.execSQL(
            "UPDATE outbox SET attempts = attempts + 1, last_sent_at_ms = ? WHERE server_id = ? AND client_msg_id = ?",
            arrayOf<Any>(atMs, serverId, clientMsgId),
        )
    }

    override fun closeOutbox(serverId: Long, clientMsgId: String, result: String, atMs: Long) {
        db.update(
            "outbox", ContentValues().apply { put("result", result); put("closed_at_ms", atMs) },
            "server_id = ? AND client_msg_id = ? AND closed_at_ms IS NULL", arrayOf(serverId.toString(), clientMsgId),
        )
    }

    override fun closeOutboxOf(serverId: Long, requestId: String, result: String, atMs: Long) {
        db.update(
            "outbox", ContentValues().apply { put("result", result); put("closed_at_ms", atMs) },
            "server_id = ? AND request_id = ? AND closed_at_ms IS NULL", arrayOf(serverId.toString(), requestId),
        )
    }

    // ------------------------------------------------------------ local retention

    override fun deleteItem(serverId: Long, requestId: String) {
        val args = arrayOf(serverId.toString(), requestId)
        for (table in listOf("items", "item_turns", "item_settled_early", "outbox")) {
            db.delete(table, "server_id = ? AND request_id = ?", args)
        }
    }

    override fun deleteClosedAgentActionsBefore(serverId: Long, beforeMs: Long) {
        db.delete(
            "outbox", "server_id = ? AND type = ? AND closed_at_ms IS NOT NULL AND closed_at_ms < ?",
            arrayOf(serverId.toString(), OutboxType.AGENT_DO.wire, beforeMs.toString()),
        )
    }

    override fun deleteDeliveriesBefore(serverId: Long, beforeMs: Long) {
        db.delete("deliveries", "server_id = ? AND received_at_ms < ?", arrayOf(serverId.toString(), beforeMs.toString()))
    }

    // ------------------------------------------------------------ agents

    private fun readAgent(c: Cursor): Agent = Agent.of(
        serverId = c.getLong(0), f = ServerFrames.decode(c.getString(1)) as AgentFrame, receivedAtMs = c.getLong(2), notified = c.getInt(3) != 0,
    )

    private fun queryAgents(sql: String, args: Array<String>): List<Agent> =
        db.rawQuery(sql, args).use { c -> buildList { while (c.moveToNext()) add(readAgent(c)) } }

    override fun agent(serverId: Long, agentId: String): Agent? = queryAgents(
        "SELECT server_id, frame, received_at_ms, notified FROM agents WHERE server_id = ? AND agent_id = ?",
        arrayOf(serverId.toString(), agentId),
    ).firstOrNull()

    override fun putAgent(agent: Agent) {
        db.insertWithOnConflict(
            "agents", null,
            ContentValues().apply {
                put("server_id", agent.serverId)
                put("agent_id", agent.agentId)
                put("rev", agent.rev)
                put("frame", StrictJson.compact(agent.toFrame(Agent.NO_DELIVERY).toJson()))
                put("received_at_ms", agent.receivedAtMs)
                put("notified", if (agent.notified) 1 else 0)
            },
            SQLiteDatabase.CONFLICT_REPLACE,
        )
    }

    override fun agents(serverId: Long): List<Agent> = queryAgents(
        "SELECT server_id, frame, received_at_ms, notified FROM agents WHERE server_id = ? ORDER BY agent_id", arrayOf(serverId.toString()),
    )

    override fun allAgents(): List<Agent> = queryAgents("SELECT server_id, frame, received_at_ms, notified FROM agents ORDER BY server_id, agent_id", emptyArray())

    override fun markAgentNotified(serverId: Long, agentId: String) {
        db.update(
            "agents", ContentValues().apply { put("notified", 1) },
            "server_id = ? AND agent_id = ?", arrayOf(serverId.toString(), agentId),
        )
    }

    // ------------------------------------------------------------ preferences

    override fun prefs(serverId: Long): StoredPrefs? = db.rawQuery(
        "SELECT prefs, body_purge_after_ms FROM server_prefs WHERE server_id = ?", arrayOf(serverId.toString()),
    ).use { c ->
        if (!c.moveToFirst()) null
        else StoredPrefs(prefsFromJson(c.getString(0)), c.getLong(1))
    }

    override fun putPrefs(serverId: Long, prefs: StoredPrefs, atMs: Long) {
        db.insertWithOnConflict(
            "server_prefs", null,
            ContentValues().apply {
                put("server_id", serverId)
                put("prefs", StrictJson.compact(prefs.prefs.toJson()))
                put("body_purge_after_ms", prefs.bodyPurgeAfterMs)
                put("updated_at_ms", atMs)
            },
            SQLiteDatabase.CONFLICT_REPLACE,
        )
    }

    companion object {
        fun viewToJson(v: ResolvedView): String = StrictJson.compact(
            JSONObject().put("title", v.title)
                .put("fields", JSONArray(v.fields.map { JSONObject().put("label", it.label).put("value", it.value) }))
                .put("body", v.body).putOpt("body_label", v.bodyLabel),
        )

        fun viewFromJson(text: String): ResolvedView {
            val o = StrictJson.parseObject(text)
            val fields = o.getJSONArray("fields")
            return ResolvedView(
                o.getString("title"),
                (0 until fields.length()).map { ResolvedField(fields.getJSONObject(it).getString("label"), fields.getJSONObject(it).getString("value")) },
                o.getString("body"),
                if (o.has("body_label")) o.getString("body_label") else null,
            )
        }

        /** The preferences as the `welcome` frame holds them. They were checked when they came in. */
        fun prefsFromJson(text: String): Prefs = Prefs.fromJson(StrictJson.parseObject(text))
    }
}
