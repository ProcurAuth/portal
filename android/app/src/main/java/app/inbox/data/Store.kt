package app.inbox.data

import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteOpenHelper

data class Server(
    val id: Long,
    val address: String,
    val serverName: String,
    val deviceId: String,
    val deviceName: String,
    val deviceKeySealed: ByteArray,
    val serverPublicKey: ByteArray,
    val reenrollReason: String?,
    val heartbeatMs: Long,
    val offlineWarningMs: Long,
    /** Pseudo under which this phone is tracked on that server (null: none). */
    val userName: String?,
)

data class Group(val id: String, val name: String, val description: String)

data class Alert(
    val serverId: Long,
    val alertId: String,
    val group: String,
    val groupName: String,
    /** Short summary (at most 128 characters on the server). */
    val description: String?,
    /** Long Markdown text. */
    val details: String?,
    val createdAtMs: Long,
    val receivedAtMs: Long,
    /** What this phone did with it: the local level of its group when it arrived. */
    val action: String,
    val notified: Boolean,
    /** When the alarm sound was stopped on this phone (local only, never sent). */
    val stoppedAtMs: Long?,
    /** When the user marked it read on this phone (local only, never sent). */
    val readAtMs: Long?,
) {
    val unread: Boolean get() = readAtMs == null
}

/**
 * Local durable state. Alerts are keyed by (server, alert_id): storing an alert is
 * the deduplication point that guarantees each alert is shown exactly once.
 * Alerts are one-way: the only thing sent back is the automatic transport receipt.
 * The `acked_at_ms` column holds the local "alarm stopped" time, `read_at_ms` the
 * local "marked read" time. Neither is ever sent.
 *
 * Column names predate protocol v2: `alerts.title` holds the description and
 * `alerts.message` the Markdown details; `subscriptions` holds this phone's own
 * level per group (alarm / ring / notify, or off). The server only knows which
 * groups this phone subscribes to, never the level.
 */
class Store private constructor(context: Context, private val defaultUserName: String?) :
    SQLiteOpenHelper(context, "inbox.db", null, 4) {

    companion object {
        @Volatile private var instance: Store? = null

        fun get(context: Context): Store =
            instance ?: synchronized(this) {
                instance ?: Store(
                    context.applicationContext,
                    // Only needed by the v4 migration (servers enrolled without a username).
                    context.getSharedPreferences("inbox_ui", Context.MODE_PRIVATE).getString("default_user_name", null),
                ).also { instance = it }
            }

        const val LEVEL_OFF = "off"
        const val LEVEL_NOTIFY = "notify"
        const val DEFAULT_HEARTBEAT_MS = 20_000L
        const val DEFAULT_OFFLINE_WARNING_MS = 180_000L
    }

    override fun onConfigure(db: SQLiteDatabase) {
        db.setForeignKeyConstraintsEnabled(true)
        db.enableWriteAheadLogging()
    }

    override fun onCreate(db: SQLiteDatabase) {
        db.execSQL(
            """CREATE TABLE servers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                address TEXT NOT NULL,
                server_name TEXT NOT NULL,
                device_id TEXT NOT NULL,
                device_name TEXT NOT NULL,
                device_key BLOB NOT NULL,
                server_public_key BLOB NOT NULL,
                reenroll_reason TEXT,
                heartbeat_ms INTEGER NOT NULL,
                offline_warning_ms INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                user_name TEXT)"""
        )
        db.execSQL(
            """CREATE TABLE groups (
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                group_id TEXT NOT NULL, name TEXT NOT NULL, description TEXT NOT NULL,
                PRIMARY KEY (server_id, group_id))"""
        )
        db.execSQL(
            """CREATE TABLE subscriptions (
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                group_id TEXT NOT NULL, action TEXT NOT NULL,
                PRIMARY KEY (server_id, group_id))"""
        )
        db.execSQL(
            """CREATE TABLE alerts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                alert_id TEXT NOT NULL,
                group_id TEXT NOT NULL,
                group_name TEXT NOT NULL,
                title TEXT,
                message TEXT,
                created_at_ms INTEGER NOT NULL,
                received_at_ms INTEGER NOT NULL,
                action TEXT NOT NULL,
                notified INTEGER NOT NULL DEFAULT 0,
                acked_at_ms INTEGER,
                read_at_ms INTEGER,
                UNIQUE (server_id, alert_id))"""
        )
        db.execSQL("CREATE INDEX alerts_recent ON alerts(server_id, received_at_ms)")
    }

    override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) {
        // v2: pseudo per server. Existing servers keep working without one.
        if (oldVersion < 2) db.execSQL("ALTER TABLE servers ADD COLUMN user_name TEXT")
        // v3: local read state. Alerts received before the upgrade count as read.
        if (oldVersion < 3) {
            db.execSQL("ALTER TABLE alerts ADD COLUMN read_at_ms INTEGER")
            db.execSQL("UPDATE alerts SET read_at_ms = received_at_ms")
        }
        // v4: the receipt is sent right after storing (the server retransmits until
        // it gets one), so the ack outbox is gone; servers enrolled without a
        // username take the default one.
        if (oldVersion < 4) {
            db.execSQL("DROP TABLE IF EXISTS outbox")
            defaultUserName?.takeIf { it.isNotBlank() }
                ?.let { db.execSQL("UPDATE servers SET user_name = ? WHERE user_name IS NULL", arrayOf(it)) }
        }
    }

    // ------------------------------------------------------------ servers

    fun addServer(
        address: String, serverName: String, deviceId: String, deviceName: String,
        deviceKeySealed: ByteArray, serverPublicKey: ByteArray, userName: String?,
    ): Long {
        val v = ContentValues().apply {
            if (userName == null) putNull("user_name") else put("user_name", userName)
            put("address", address)
            put("server_name", serverName)
            put("device_id", deviceId)
            put("device_name", deviceName)
            put("device_key", deviceKeySealed)
            put("server_public_key", serverPublicKey)
            put("heartbeat_ms", DEFAULT_HEARTBEAT_MS)
            put("offline_warning_ms", DEFAULT_OFFLINE_WARNING_MS)
            put("created_at", System.currentTimeMillis())
        }
        return writableDatabase.insertOrThrow("servers", null, v)
    }

    fun deleteServer(id: Long) {
        writableDatabase.delete("servers", "id = ?", arrayOf(id.toString()))
    }

    fun servers(): List<Server> = readableDatabase.rawQuery(
        "SELECT id, address, server_name, device_id, device_name, device_key, server_public_key, " +
            "reenroll_reason, heartbeat_ms, offline_warning_ms, user_name FROM servers ORDER BY id", null
    ).use { c ->
        buildList {
            while (c.moveToNext()) {
                add(
                    Server(
                        c.getLong(0), c.getString(1), c.getString(2), c.getString(3), c.getString(4),
                        c.getBlob(5), c.getBlob(6), if (c.isNull(7)) null else c.getString(7),
                        c.getLong(8), c.getLong(9), if (c.isNull(10)) null else c.getString(10),
                    )
                )
            }
        }
    }

    fun server(id: Long): Server? = servers().firstOrNull { it.id == id }

    fun setUserName(id: Long, userName: String?) {
        val v = ContentValues().apply { if (userName == null) putNull("user_name") else put("user_name", userName) }
        writableDatabase.update("servers", v, "id = ?", arrayOf(id.toString()))
    }

    fun setReenroll(id: Long, reason: String?) {
        val v = ContentValues().apply { if (reason == null) putNull("reenroll_reason") else put("reenroll_reason", reason) }
        writableDatabase.update("servers", v, "id = ?", arrayOf(id.toString()))
    }

    /** What the session must send so the server's subscriptions match the local levels. */
    data class Reconcile(val subscribe: List<String>, val unsubscribe: List<String>)

    /**
     * Stores the server's groups and reconciles the local levels with the groups the
     * server says this phone subscribes to: a subscribed group without a local level
     * gets NOTIF; a local level the server does not know is subscribed again; a group
     * turned off while offline is unsubscribed.
     */
    fun saveWelcome(
        id: Long, serverName: String, groups: List<Group>, serverSubscriptions: Set<String>,
        heartbeatMs: Long, offlineWarningMs: Long,
    ): Reconcile {
        val db = writableDatabase
        db.beginTransaction()
        try {
            db.update("servers", ContentValues().apply {
                put("server_name", serverName)
                put("heartbeat_ms", heartbeatMs)
                put("offline_warning_ms", offlineWarningMs)
            }, "id = ?", arrayOf(id.toString()))
            db.delete("groups", "server_id = ?", arrayOf(id.toString()))
            for (g in groups) {
                db.insertOrThrow("groups", null, ContentValues().apply {
                    put("server_id", id); put("group_id", g.id); put("name", g.name); put("description", g.description)
                })
            }
            val known = groups.map { it.id }.toSet()
            val levels = levels(id).filterKeys { it in known }
            db.delete("subscriptions", "server_id = ?", arrayOf(id.toString()))
            val merged = levels.toMutableMap()
            for (g in serverSubscriptions) if (g in known && g !in merged) merged[g] = LEVEL_NOTIFY
            for ((g, a) in merged) insertLevel(db, id, g, a)
            db.setTransactionSuccessful()
            return Reconcile(
                subscribe = merged.filter { it.value != LEVEL_OFF && it.key !in serverSubscriptions }.keys.sorted(),
                unsubscribe = merged.filter { it.value == LEVEL_OFF && it.key in serverSubscriptions }.keys.sorted(),
            )
        } finally {
            db.endTransaction()
        }
    }

    private fun insertLevel(db: SQLiteDatabase, id: Long, group: String, action: String) {
        db.insertWithOnConflict("subscriptions", null, ContentValues().apply {
            put("server_id", id); put("group_id", group); put("action", action)
        }, SQLiteDatabase.CONFLICT_REPLACE)
    }

    /** Sets this phone's level for a group; null turns the group off. */
    fun setLevel(id: Long, group: String, action: String?) {
        insertLevel(writableDatabase, id, group, action ?: LEVEL_OFF)
    }

    fun groups(serverId: Long): List<Group> = readableDatabase.rawQuery(
        "SELECT group_id, name, description FROM groups WHERE server_id = ? ORDER BY name",
        arrayOf(serverId.toString())
    ).use { c -> buildList { while (c.moveToNext()) add(Group(c.getString(0), c.getString(1), c.getString(2))) } }

    /** Local level of every group with one, including explicit "off". */
    fun levels(serverId: Long): Map<String, String> = readableDatabase.rawQuery(
        "SELECT group_id, action FROM subscriptions WHERE server_id = ?", arrayOf(serverId.toString())
    ).use { c -> buildMap { while (c.moveToNext()) put(c.getString(0), c.getString(1)) } }

    /** Groups this phone is subscribed to, with their level (off groups excluded). */
    fun subscriptions(serverId: Long): Map<String, String> = levels(serverId).filterValues { it != LEVEL_OFF }

    // ------------------------------------------------------------ alerts

    /** Stores the alert durably. Returns false if it was already stored (a retransmission). */
    fun insertAlertIfNew(a: Alert): Boolean {
        val v = ContentValues().apply {
            put("server_id", a.serverId)
            put("alert_id", a.alertId)
            put("group_id", a.group)
            put("group_name", a.groupName)
            put("title", a.description)
            put("message", a.details)
            put("created_at_ms", a.createdAtMs)
            put("received_at_ms", a.receivedAtMs)
            put("action", a.action)
            put("notified", 0)
        }
        return writableDatabase.insertWithOnConflict("alerts", null, v, SQLiteDatabase.CONFLICT_IGNORE) != -1L
    }

    fun markNotified(serverId: Long, alertId: String) {
        writableDatabase.update(
            "alerts", ContentValues().apply { put("notified", 1) },
            "server_id = ? AND alert_id = ?", arrayOf(serverId.toString(), alertId)
        )
    }

    /** Records that the alarm sound was stopped on this phone. Returns false if it already was. */
    fun markStopped(serverId: Long, alertId: String): Boolean =
        writableDatabase.update(
            "alerts", ContentValues().apply { put("acked_at_ms", System.currentTimeMillis()) },
            "server_id = ? AND alert_id = ? AND acked_at_ms IS NULL", arrayOf(serverId.toString(), alertId)
        ) > 0

    private fun readAlerts(sql: String, args: Array<String>): List<Alert> =
        readableDatabase.rawQuery(sql, args).use { c ->
            buildList {
                while (c.moveToNext()) {
                    add(
                        Alert(
                            c.getLong(0), c.getString(1), c.getString(2), c.getString(3),
                            if (c.isNull(4)) null else c.getString(4),
                            if (c.isNull(5)) null else c.getString(5),
                            c.getLong(6), c.getLong(7), c.getString(8), c.getInt(9) != 0,
                            if (c.isNull(10)) null else c.getLong(10),
                            if (c.isNull(11)) null else c.getLong(11),
                        )
                    )
                }
            }
        }

    private val alertColumns =
        "server_id, alert_id, group_id, group_name, title, message, created_at_ms, received_at_ms, action, notified, acked_at_ms, read_at_ms"

    fun alert(serverId: Long, alertId: String): Alert? = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE server_id = ? AND alert_id = ?",
        arrayOf(serverId.toString(), alertId)
    ).firstOrNull()

    private fun readFilter(includeRead: Boolean) = if (includeRead) "" else " AND read_at_ms IS NULL"

    /**
     * The `limit` most recent alerts of every server received since `sinceMs`, newest
     * first; unread ones only unless `includeRead`.
     */
    fun recentAlerts(sinceMs: Long, limit: Int, includeRead: Boolean): List<Alert> = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE received_at_ms >= ?${readFilter(includeRead)} " +
            "ORDER BY received_at_ms DESC, id DESC LIMIT ?",
        arrayOf(sinceMs.toString(), limit.toString())
    )

    /** The `limit` most recent alerts of every server, whatever their age, newest first. */
    fun latestAlerts(limit: Int, includeRead: Boolean): List<Alert> = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE 1 = 1${readFilter(includeRead)} " +
            "ORDER BY received_at_ms DESC, id DESC LIMIT ?",
        arrayOf(limit.toString())
    )

    fun alertCount(includeRead: Boolean): Int = readableDatabase
        .rawQuery("SELECT COUNT(*) FROM alerts WHERE 1 = 1${readFilter(includeRead)}", null)
        .use { c -> if (c.moveToFirst()) c.getInt(0) else 0 }

    /** Marks one alert read. Returns false if it already was. */
    fun markRead(serverId: Long, alertId: String): Boolean =
        writableDatabase.update(
            "alerts", ContentValues().apply { put("read_at_ms", System.currentTimeMillis()) },
            "server_id = ? AND alert_id = ? AND read_at_ms IS NULL", arrayOf(serverId.toString(), alertId)
        ) > 0

    /** Undo of a "mark read": the alert counts as unread again (local state only). */
    fun markUnread(serverId: Long, alertId: String) {
        writableDatabase.update(
            "alerts", ContentValues().apply { putNull("read_at_ms") },
            "server_id = ? AND alert_id = ?", arrayOf(serverId.toString(), alertId)
        )
    }

    fun unreadAlerts(): List<Alert> = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE read_at_ms IS NULL ORDER BY received_at_ms", emptyArray()
    )

    fun unreadCount(): Int = readableDatabase.rawQuery(
        "SELECT COUNT(*) FROM alerts WHERE read_at_ms IS NULL", null
    ).use { c -> if (c.moveToFirst()) c.getInt(0) else 0 }

    fun serverAlerts(serverId: Long): List<Alert> = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE server_id = ?", arrayOf(serverId.toString())
    )

    /** Stored but never shown: the app died between storing and notifying. */
    fun unnotified(): List<Alert> = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE notified = 0 ORDER BY received_at_ms", emptyArray()
    )

    /** Alarms whose sound has not been stopped on this phone. */
    fun ringingAlarms(): List<Alert> = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE action = 'alarm' AND acked_at_ms IS NULL ORDER BY received_at_ms",
        emptyArray()
    )

    /** Keeps the local history bounded. Alarms still ringing are never removed. */
    fun prune(olderThanMs: Long) {
        writableDatabase.delete(
            "alerts", "received_at_ms < ? AND notified = 1 AND (action != 'alarm' OR acked_at_ms IS NOT NULL)",
            arrayOf(olderThanMs.toString())
        )
    }
}
