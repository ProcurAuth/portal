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
    val title: String?,
    val message: String?,
    val createdAtMs: Long,
    val receivedAtMs: Long,
    val action: String,
    val notified: Boolean,
    /** When the alarm sound was stopped on this phone (local only, never sent). */
    val stoppedAtMs: Long?,
)

data class OutboxItem(val id: Long, val serverId: Long, val alertId: String, val stage: String)

/**
 * Local durable state. Alerts are keyed by (server, alert_id): storing an alert is
 * the deduplication point that guarantees each alert is shown exactly once.
 * Alerts are one-way: the only thing sent back is the automatic transport receipt.
 * The `acked_at_ms` column holds the local "alarm stopped" time.
 */
class Store private constructor(context: Context) :
    SQLiteOpenHelper(context, "inbox.db", null, 2) {

    companion object {
        @Volatile private var instance: Store? = null

        fun get(context: Context): Store =
            instance ?: synchronized(this) {
                instance ?: Store(context.applicationContext).also { instance = it }
            }

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
                UNIQUE (server_id, alert_id))"""
        )
        db.execSQL("CREATE INDEX alerts_recent ON alerts(server_id, received_at_ms)")
        db.execSQL(
            """CREATE TABLE outbox (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                alert_id TEXT NOT NULL,
                stage TEXT NOT NULL,
                UNIQUE (server_id, alert_id, stage))"""
        )
    }

    override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) {
        // v2: pseudo per server. Existing servers keep working without one.
        if (oldVersion < 2) db.execSQL("ALTER TABLE servers ADD COLUMN user_name TEXT")
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

    fun saveWelcome(
        id: Long, serverName: String, groups: List<Group>, subscriptions: Map<String, String>,
        heartbeatMs: Long, offlineWarningMs: Long,
    ) {
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
            replaceSubscriptions(db, id, subscriptions)
            db.setTransactionSuccessful()
        } finally {
            db.endTransaction()
        }
    }

    fun saveSubscriptions(id: Long, subscriptions: Map<String, String>) {
        val db = writableDatabase
        db.beginTransaction()
        try {
            replaceSubscriptions(db, id, subscriptions)
            db.setTransactionSuccessful()
        } finally {
            db.endTransaction()
        }
    }

    private fun replaceSubscriptions(db: SQLiteDatabase, id: Long, subscriptions: Map<String, String>) {
        db.delete("subscriptions", "server_id = ?", arrayOf(id.toString()))
        for ((g, a) in subscriptions) {
            db.insertOrThrow("subscriptions", null, ContentValues().apply {
                put("server_id", id); put("group_id", g); put("action", a)
            })
        }
    }

    fun groups(serverId: Long): List<Group> = readableDatabase.rawQuery(
        "SELECT group_id, name, description FROM groups WHERE server_id = ? ORDER BY name",
        arrayOf(serverId.toString())
    ).use { c -> buildList { while (c.moveToNext()) add(Group(c.getString(0), c.getString(1), c.getString(2))) } }

    fun subscriptions(serverId: Long): Map<String, String> = readableDatabase.rawQuery(
        "SELECT group_id, action FROM subscriptions WHERE server_id = ?", arrayOf(serverId.toString())
    ).use { c -> buildMap { while (c.moveToNext()) put(c.getString(0), c.getString(1)) } }

    // ------------------------------------------------------------ alerts

    /** Stores the alert durably. Returns false if it was already stored (a retransmission). */
    fun insertAlertIfNew(a: Alert): Boolean {
        val v = ContentValues().apply {
            put("server_id", a.serverId)
            put("alert_id", a.alertId)
            put("group_id", a.group)
            put("group_name", a.groupName)
            put("title", a.title)
            put("message", a.message)
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
                        )
                    )
                }
            }
        }

    private val alertColumns =
        "server_id, alert_id, group_id, group_name, title, message, created_at_ms, received_at_ms, action, notified, acked_at_ms"

    fun alert(serverId: Long, alertId: String): Alert? = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE server_id = ? AND alert_id = ?",
        arrayOf(serverId.toString(), alertId)
    ).firstOrNull()

    /** The `limit` most recent alerts of every server received since `sinceMs`, newest first. */
    fun recentAlerts(sinceMs: Long, limit: Int): List<Alert> = readAlerts(
        "SELECT $alertColumns FROM alerts WHERE received_at_ms >= ? ORDER BY received_at_ms DESC, id DESC LIMIT ?",
        arrayOf(sinceMs.toString(), limit.toString())
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

    // ------------------------------------------------------------ outbox

    fun enqueueAck(serverId: Long, alertId: String, stage: String) {
        writableDatabase.insertWithOnConflict("outbox", null, ContentValues().apply {
            put("server_id", serverId); put("alert_id", alertId); put("stage", stage)
        }, SQLiteDatabase.CONFLICT_IGNORE)
    }

    fun outbox(serverId: Long): List<OutboxItem> = readableDatabase.rawQuery(
        "SELECT id, server_id, alert_id, stage FROM outbox WHERE server_id = ? ORDER BY id LIMIT 256",
        arrayOf(serverId.toString())
    ).use { c -> buildList { while (c.moveToNext()) add(OutboxItem(c.getLong(0), c.getLong(1), c.getString(2), c.getString(3))) } }

    fun removeOutbox(id: Long) {
        writableDatabase.delete("outbox", "id = ?", arrayOf(id.toString()))
    }

    /** Keeps the local history bounded. Alarms still ringing are never removed. */
    fun prune(olderThanMs: Long) {
        writableDatabase.delete(
            "alerts", "received_at_ms < ? AND notified = 1 AND (action != 'alarm' OR acked_at_ms IS NOT NULL)",
            arrayOf(olderThanMs.toString())
        )
    }
}
