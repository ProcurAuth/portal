package app.portalinbox.data

import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteConstraintException
import android.database.sqlite.SQLiteOpenHelper
import app.portalinbox.net.ServerAddress
import app.portalinbox.portal.Fingerprint
import app.portalinbox.portal.KnownPortal
import app.portalinbox.portal.LastSync
import app.portalinbox.portal.PortalAdmission
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PortalNaming
import app.portalinbox.portal.PortalOrder
import app.portalinbox.portal.PortalPalette
import java.util.concurrent.ConcurrentHashMap

data class Group(val id: String, val name: String, val description: String)

data class Alert(
    val serverId: Long,
    val alertId: String,
    val group: String,
    val groupName: String,
    /** The kind of the message: `alert` today. See [Kind]. */
    val kind: String,
    /** Single line, at most 128 characters on the server. */
    val title: String,
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
 * The items of protocol 5 (questions and approvals), their turns and the outbox of answers are in
 * the tables of [ItemSchema]. [SqlItemRepository] reads and writes them for the `ItemEngine`.
 *
 * `subscriptions` holds this phone's own level per group (alarm / ring / notify,
 * or off). The server only knows which groups this phone subscribes to, never the
 * level.
 */
class Store private constructor(private val appContext: Context) :
    SQLiteOpenHelper(appContext, "inbox.db", null, 9) {

    companion object {
        @Volatile private var instance: Store? = null

        fun get(context: Context): Store =
            instance ?: synchronized(this) {
                instance ?: Store(context.applicationContext).also { instance = it }
            }

        const val LEVEL_OFF = Levels.OFF
        const val LEVEL_NOTIFY = Levels.NOTIFY
        const val DEFAULT_HEARTBEAT_MS = 20_000L
        const val DEFAULT_OFFLINE_WARNING_MS = 180_000L

        /** Preference key (file `inbox_ui`) set when an update removed servers of an older protocol. */
        const val PREF_ENROLL_AGAIN = "enroll_again"
    }

    override fun onConfigure(db: SQLiteDatabase) {
        db.setForeignKeyConstraintsEnabled(true)
        db.enableWriteAheadLogging()
    }

    override fun onCreate(db: SQLiteDatabase) {
        createServers(db)
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
        createAlerts(db)
        ItemSchema.create(db)
    }

    /**
     * The identity columns are local (the user chose them, the pinned key gives the fingerprint). `nickname_key`, `color` and
     * `key_fp` are `UNIQUE`: the database refuses a second portal with the same key, name or color, if the code of
     * [PortalAdmission] ever missed one. `reported_name` is what the portal says about itself: it is not an identity.
     */
    private fun createServers(db: SQLiteDatabase) {
        db.execSQL(
            """CREATE TABLE servers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                address TEXT NOT NULL,
                reported_name TEXT NOT NULL,
                device_id TEXT NOT NULL,
                device_name TEXT NOT NULL,
                device_key BLOB NOT NULL,
                server_public_key BLOB NOT NULL,
                reenroll_reason TEXT,
                heartbeat_ms INTEGER NOT NULL,
                offline_warning_ms INTEGER NOT NULL,
                user_name TEXT NOT NULL,
                nickname TEXT NOT NULL,
                nickname_key TEXT NOT NULL UNIQUE,
                color INTEGER NOT NULL UNIQUE CHECK (color BETWEEN 0 AND 9),
                sort_index INTEGER NOT NULL,
                key_fp TEXT NOT NULL UNIQUE,
                muted_until_ms INTEGER,
                hide_name_on_lock INTEGER NOT NULL DEFAULT 0,
                retention_days INTEGER,
                last_sync_ms INTEGER,
                prev_key_fp TEXT,
                key_changed_ms INTEGER,
                pending_prefs TEXT,
                added_at_ms INTEGER NOT NULL)"""
        )
    }

    private fun createAlerts(db: SQLiteDatabase) {
        db.execSQL(
            """CREATE TABLE alerts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                server_id INTEGER NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
                alert_id TEXT NOT NULL,
                group_id TEXT NOT NULL,
                group_name TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'alert',
                title TEXT NOT NULL,
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
        // v9: the identity of a portal is local (nickname, color, pinned key fingerprint). Every earlier row has none, and no
        // phone holds data worth a migration: all tables are dropped and made again, and the person is told to enroll again.
        // Older versions (v6 protocol 4, v7 the column `kind`, v8 protocol 5) used the same removal.
        if (oldVersion < 9) {
            val removed = try {
                db.rawQuery("SELECT COUNT(*) FROM servers", null).use { c -> if (c.moveToFirst()) c.getInt(0) else 0 }
            } catch (e: android.database.sqlite.SQLiteException) {
                0
            }
            ItemSchema.drop(db)
            for (table in listOf("alerts", "subscriptions", "groups", "servers")) {
                db.execSQL("DROP TABLE IF EXISTS $table")
            }
            onCreate(db)
            if (removed > 0) {
                appContext.getSharedPreferences("inbox_ui", Context.MODE_PRIVATE)
                    .edit().putBoolean(PREF_ENROLL_AGAIN, true).apply()
            }
        }
    }

    // ------------------------------------------------------------ servers

    /** What the enrollment gives and what the user chose. The nickname is required: the phone never takes the name of the portal. */
    class NewPortal(
        val address: String,
        val reportedName: String,
        val deviceId: String,
        val deviceName: String,
        val deviceKeySealed: ByteArray,
        val serverPublicKey: ByteArray,
        val userName: String,
        val nickname: String,
        /** Index of the palette, or null for the first free color. */
        val colorIndex: Int?,
    )

    sealed interface AddResult {
        data class Added(val id: Long) : AddResult

        /** [PortalAdmission] refused: same key, 10 portals, bad or taken name, bad or taken color. Nothing was written. */
        data class Refused(val refusal: PortalAdmission.Refusal) : AddResult

        /** The `UNIQUE` columns refused a row that [PortalAdmission] accepted (two adds at once). Nothing was written. */
        object Duplicate : AddResult
    }

    /**
     * Adds a portal, or refuses. The check and the insert run in one transaction. The key to pin is the key of the
     * enrollment: its fingerprint is derived here, once, and stored in `key_fp`.
     */
    fun addServer(p: NewPortal): AddResult {
        val fp = Fingerprint.compactOf(p.serverPublicKey)
        val db = writableDatabase
        db.beginTransaction()
        try {
            val decision = PortalAdmission.check(knownPortals(), fp, p.nickname, p.colorIndex)
            val admit = when (decision) {
                is PortalAdmission.Decision.Refuse -> return AddResult.Refused(decision.refusal)
                is PortalAdmission.Decision.Admit -> decision
            }
            val v = ContentValues().apply {
                put("user_name", p.userName)
                put("address", p.address)
                put("reported_name", p.reportedName)
                put("device_id", p.deviceId)
                put("device_name", p.deviceName)
                put("device_key", p.deviceKeySealed)
                put("server_public_key", p.serverPublicKey)
                put("heartbeat_ms", DEFAULT_HEARTBEAT_MS)
                put("offline_warning_ms", DEFAULT_OFFLINE_WARNING_MS)
                put("nickname", admit.nickname)
                put("nickname_key", admit.nicknameKey)
                put("color", admit.colorIndex)
                put("sort_index", nextSortIndex(db))
                put("key_fp", fp)
                put("added_at_ms", System.currentTimeMillis())
            }
            val id = db.insertOrThrow("servers", null, v)
            db.setTransactionSuccessful()
            return AddResult.Added(id)
        } catch (e: SQLiteConstraintException) {
            return AddResult.Duplicate
        } finally {
            db.endTransaction()
        }
    }

    private fun nextSortIndex(db: SQLiteDatabase): Int =
        db.rawQuery("SELECT COALESCE(MAX(sort_index), -1) + 1 FROM servers", null).use { c -> if (c.moveToFirst()) c.getInt(0) else 0 }

    /** Removes the portal. Its rows, its notifications' data and its color and nickname go with it: both can be used again at once. */
    fun deleteServer(id: Long) {
        writableDatabase.delete("servers", "id = ?", arrayOf(id.toString()))
        lastSyncWritten.remove(id)
    }

    private val serverColumns =
        "id, address, reported_name, device_id, device_name, device_key, server_public_key, reenroll_reason, heartbeat_ms, " +
            "offline_warning_ms, user_name, nickname, color, sort_index, key_fp, muted_until_ms, hide_name_on_lock, " +
            "retention_days, last_sync_ms, prev_key_fp, key_changed_ms, pending_prefs, added_at_ms"

    /** The portals in the order of the user. */
    fun servers(): List<Server> = readableDatabase.rawQuery(
        "SELECT $serverColumns FROM servers ORDER BY sort_index, id", null
    ).use { c ->
        buildList {
            while (c.moveToNext()) {
                add(
                    Server(
                        id = c.getLong(0), address = c.getString(1), reportedName = c.getString(2), deviceId = c.getString(3),
                        deviceName = c.getString(4), deviceKeySealed = c.getBlob(5), serverPublicKey = c.getBlob(6),
                        reenrollReason = c.getStringOrNull(7), heartbeatMs = c.getLong(8), offlineWarningMs = c.getLong(9),
                        userName = c.getString(10), nickname = c.getString(11), colorIndex = c.getInt(12), sortIndex = c.getInt(13),
                        keyFp = c.getString(14), mutedUntilMs = c.getLongOrNull(15), hideNameOnLock = c.getInt(16) != 0,
                        retentionDays = c.getIntOrNull(17), lastSyncMs = c.getLongOrNull(18), prevKeyFp = c.getStringOrNull(19),
                        keyChangedMs = c.getLongOrNull(20), pendingPrefs = c.getStringOrNull(21), addedAtMs = c.getLong(22),
                    )
                )
            }
        }
    }

    private fun android.database.Cursor.getStringOrNull(i: Int): String? = if (isNull(i)) null else getString(i)
    private fun android.database.Cursor.getLongOrNull(i: Int): Long? = if (isNull(i)) null else getLong(i)
    private fun android.database.Cursor.getIntOrNull(i: Int): Int? = if (isNull(i)) null else getInt(i)

    fun server(id: Long): Server? = servers().firstOrNull { it.id == id }

    /** The identity of every portal, in the order of the user. */
    fun portals(): List<PortalIdentity> = servers().map { PortalIdentity.of(it) }

    /** The portals with their address, in the order of addition (the last one is the newest): what the admission checks compare to. */
    fun knownPortals(): List<KnownPortal> = servers().sortedBy { it.id }.map { KnownPortal.of(it) }

    fun setReenroll(id: Long, reason: String?) {
        val v = ContentValues().apply { if (reason == null) putNull("reenroll_reason") else put("reenroll_reason", reason) }
        writableDatabase.update("servers", v, "id = ?", arrayOf(id.toString()))
    }

    /** Renames a portal. Returns the check: only a valid and unique name is written. */
    fun rename(id: Long, nickname: String): PortalNaming.Check {
        val check = PortalNaming.validate(nickname, portals(), ignoreId = id)
        if (check is PortalNaming.Check.Valid) {
            try {
                writableDatabase.update("servers", ContentValues().apply {
                    put("nickname", check.nickname); put("nickname_key", check.key)
                }, "id = ?", arrayOf(id.toString()))
            } catch (e: SQLiteConstraintException) {
                return PortalNaming.Check.Invalid(PortalNaming.Reason.TAKEN)
            }
        }
        return check
    }

    /** Gives a portal another color. Returns false, and writes nothing, when the index is not 0 to 9 or another portal has it. */
    fun setColor(id: Long, colorIndex: Int): Boolean {
        if (!PortalPalette.isValid(colorIndex)) return false
        return try {
            writableDatabase.update("servers", ContentValues().apply { put("color", colorIndex) }, "id = ?", arrayOf(id.toString())) > 0
        } catch (e: SQLiteConstraintException) {
            false
        }
    }

    /** Sets the order of the user. A portal that [ids] does not name goes last, in its old order. An unknown id is ignored. */
    fun reorder(ids: List<Long>) {
        val db = writableDatabase
        db.beginTransaction()
        try {
            val order = PortalOrder.merge(servers().map { it.id }, ids)
            order.forEachIndexed { index, id ->
                db.update("servers", ContentValues().apply { put("sort_index", index) }, "id = ?", arrayOf(id.toString()))
            }
            db.setTransactionSuccessful()
        } finally {
            db.endTransaction()
        }
    }

    /**
     * Changes the address of a portal ("Change address", proposal 3.4). The pinned key does not change: it still decides who
     * can answer. Returns false when the text is not an address. A new address that gives another key fails the handshake
     * and shows "Key changed".
     */
    fun setAddress(id: Long, address: String): Boolean {
        val parsed = ServerAddress.parse(address) ?: return false
        return writableDatabase.update("servers", ContentValues().apply { put("address", parsed.http) }, "id = ?", arrayOf(id.toString())) > 0
    }

    /**
     * Re-enrolls a portal in place (proposal 3.6): the row, the nickname, the color and the history stay. The device, the key,
     * the address and the login come from the new enrollment. If the key is not the old one, the old fingerprint goes to
     * `prev_key_fp` and the time to `key_changed_ms`. The reason that stopped the connection is cleared. A key that another
     * portal holds is refused (returns false, nothing is written).
     */
    fun reenroll(
        id: Long, address: String, reportedName: String, deviceId: String, deviceKeySealed: ByteArray,
        serverPublicKey: ByteArray, userName: String,
    ): Boolean {
        val old = server(id) ?: return false
        val fp = Fingerprint.compactOf(serverPublicKey)
        val v = ContentValues().apply {
            put("address", address); put("reported_name", reportedName); put("device_id", deviceId)
            put("device_key", deviceKeySealed); put("server_public_key", serverPublicKey); put("user_name", userName)
            put("key_fp", fp); putNull("reenroll_reason")
            if (fp != old.keyFp) { put("prev_key_fp", old.keyFp); put("key_changed_ms", System.currentTimeMillis()) }
        }
        return try {
            writableDatabase.update("servers", v, "id = ?", arrayOf(id.toString())) > 0
        } catch (e: SQLiteConstraintException) {
            false
        }
    }

    /** Mutes the portal until [untilMs] (local). Null unmutes. [Long.MAX_VALUE] mutes until the user unmutes. */
    fun setMuted(id: Long, untilMs: Long?) {
        val v = ContentValues().apply { if (untilMs == null) putNull("muted_until_ms") else put("muted_until_ms", untilMs) }
        writableDatabase.update("servers", v, "id = ?", arrayOf(id.toString()))
    }

    /** True hides the name of the portal on the lock screen (local). */
    fun setHideNameOnLock(id: Long, hide: Boolean) {
        writableDatabase.update("servers", ContentValues().apply { put("hide_name_on_lock", if (hide) 1 else 0) }, "id = ?", arrayOf(id.toString()))
    }

    /** Sets the retention of finished items of this portal, in days. Null means "same as all". */
    fun setRetentionDays(id: Long, days: Int?) {
        val v = ContentValues().apply { if (days == null) putNull("retention_days") else put("retention_days", days) }
        writableDatabase.update("servers", v, "id = ?", arrayOf(id.toString()))
    }

    /** The JSON of the preferences that wait for the next `welcome`. Null clears it. Nothing sends it in lot MP1. */
    fun setPendingPrefs(id: Long, json: String?) {
        val v = ContentValues().apply { if (json == null) putNull("pending_prefs") else put("pending_prefs", json) }
        writableDatabase.update("servers", v, "id = ?", arrayOf(id.toString()))
    }

    private val lastSyncWritten = ConcurrentHashMap<Long, Long>()

    /**
     * Records the time of the last frame of a portal. It writes at most once a minute ([LastSync.shouldWrite]) unless [force], and
     * never moves the value back. Call it for each frame: the cost of a skipped call is one map read.
     */
    fun touchSync(id: Long, atMs: Long, force: Boolean = false) {
        if (!LastSync.shouldWrite(lastSyncWritten[id], atMs, force)) return
        writableDatabase.execSQL(
            "UPDATE servers SET last_sync_ms = ? WHERE id = ? AND (last_sync_ms IS NULL OR last_sync_ms < ?)",
            arrayOf<Any>(atMs, id, atMs),
        )
        lastSyncWritten[id] = atMs
    }

    /**
     * Stores the server's groups and reconciles the local levels with the groups the
     * server says this phone subscribes to: a subscribed group without a local level
     * gets NOTIF; a local level the server does not know is subscribed again; a group
     * turned off while offline is unsubscribed. The pseudo group `@direct` keeps its local
     * level and is never part of the reconciliation: see [Levels].
     */
    fun saveWelcome(
        id: Long, reportedName: String, userName: String?, groups: List<Group>, serverSubscriptions: Set<String>,
        heartbeatMs: Long, offlineWarningMs: Long,
    ): Reconcile {
        val db = writableDatabase
        db.beginTransaction()
        try {
            db.update("servers", ContentValues().apply {
                put("reported_name", reportedName)
                // The login is assigned by the server: the Welcome keeps it current.
                if (!userName.isNullOrEmpty()) put("user_name", userName)
                put("heartbeat_ms", heartbeatMs)
                put("offline_warning_ms", offlineWarningMs)
            }, "id = ?", arrayOf(id.toString()))
            db.delete("groups", "server_id = ?", arrayOf(id.toString()))
            for (g in groups.filterNot { Levels.isDirect(it.id) }) {
                db.insertOrThrow("groups", null, ContentValues().apply {
                    put("server_id", id); put("group_id", g.id); put("name", g.name); put("description", g.description)
                })
            }
            val plan = Levels.reconcile(levels(id), groups.map { it.id }.toSet(), serverSubscriptions)
            db.delete("subscriptions", "server_id = ?", arrayOf(id.toString()))
            for ((g, a) in plan.levels) insertLevel(db, id, g, a)
            db.setTransactionSuccessful()
            return plan.reconcile
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

    /**
     * Groups of the server that this phone is subscribed to, with their level (off groups
     * excluded). The pseudo group `@direct` is not a subscription: use [levels] for it.
     */
    fun subscriptions(serverId: Long): Map<String, String> =
        levels(serverId).filter { Levels.isServerGroup(it.key) && it.value != LEVEL_OFF }

    // ------------------------------------------------------------ alerts

    /** Stores the alert durably. Returns false if it was already stored (a retransmission). */
    fun insertAlertIfNew(a: Alert): Boolean {
        val v = ContentValues().apply {
            put("server_id", a.serverId)
            put("alert_id", a.alertId)
            put("group_id", a.group)
            put("group_name", a.groupName)
            put("kind", a.kind)
            put("title", a.title)
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
                            c.getLong(0), c.getString(1), c.getString(2), c.getString(3), c.getString(4),
                            c.getString(5), c.getLong(6), c.getLong(7), c.getString(8), c.getInt(9) != 0,
                            if (c.isNull(10)) null else c.getLong(10),
                            if (c.isNull(11)) null else c.getLong(11),
                        )
                    )
                }
            }
        }

    private val alertColumns =
        "server_id, alert_id, group_id, group_name, kind, title, created_at_ms, received_at_ms, action, notified, acked_at_ms, read_at_ms"

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
