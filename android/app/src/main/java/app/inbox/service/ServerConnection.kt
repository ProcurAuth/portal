package app.inbox.service

import android.content.Context
import android.util.Log
import app.inbox.BuildConfig
import app.inbox.R
import app.inbox.alert.Alerter
import app.inbox.core.Native
import app.inbox.data.Alert
import app.inbox.data.Group
import app.inbox.data.KeyVault
import app.inbox.data.Store
import app.inbox.net.Http
import app.inbox.net.ServerAddress
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import org.json.JSONArray
import org.json.JSONObject
import java.nio.ByteBuffer
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.random.Random

/**
 * One persistent session to one server, on its own thread.
 *
 * Delivery contract (device side): an alert is written to the local database
 * before the transport receipt (`received`) is sent; a retransmitted alert is
 * recognised by its id and only receipted again; the notification is posted once.
 * Alerts are one-way: nothing else is ever sent about an alert.
 */
class ServerConnection(private val context: Context, val serverId: Long) {
    private sealed interface Event {
        object Open : Event
        class Binary(val bytes: ByteArray) : Event
        class Text(val text: String) : Event
        class Closed(val code: Int, val reason: String) : Event
        class Failure(val error: Throwable) : Event
        class Command(val json: JSONObject) : Event
        object Wake : Event
        object Stop : Event
    }

    companion object {
        private const val TAG = "Inbox.Connection"
        private const val HANDSHAKE_TIMEOUT_MS = 15_000L
        private const val MAX_BACKOFF_MS = 30_000L

        const val CLOSE_UNKNOWN_DEVICE = 4401
        const val CLOSE_REVOKED = 4403
        const val CLOSE_REPLACED = 4409
        const val CLOSE_PROTOCOL = 4426
        const val CLOSE_RATE_LIMITED = 4429
        const val CLOSE_RESTART = 1012
    }

    private val store = Store.get(context)
    private val events = LinkedBlockingQueue<Event>()
    @Volatile private var running = true
    @Volatile private var connected = false
    private var socket: WebSocket? = null
    private var session = 0L
    private var failures = 0
    private var nonce = 0L
    private val pingSentAt = sortedMapOf<Long, Long>()
    private val thread = Thread({ loop() }, "inbox-server-$serverId")

    fun start() = thread.start()

    fun stop() {
        running = false
        events.offer(Event.Stop)
    }

    /** Network became available: retry now instead of waiting for the backoff. */
    fun wake() {
        events.offer(Event.Wake)
    }

    fun isConnected() = connected

    /** Asks the server to track this phone under `userName`; sent again after each Welcome if needed. */
    fun setUserName(userName: String) {
        if (connected) events.offer(Event.Command(JSONObject().put("t", "set_user_name").put("user_name", userName)))
    }

    /**
     * Sets this phone's level for a group (null = off). The level stays on the phone:
     * the server only learns whether the group is subscribed. Offline changes are
     * applied by the next Welcome reconciliation.
     */
    fun setLevel(group: String, action: String?) {
        store.setLevel(serverId, group, action)
        val t = if (action == null) "unsubscribe" else "subscribe"
        if (connected) events.offer(Event.Command(JSONObject().put("t", t).put("group", group)))
        Registry.changed()
    }

    // ------------------------------------------------------------ main loop

    private fun loop() {
        while (running) {
            val server = store.server(serverId) ?: break
            if (server.reenrollReason != null) {
                Registry.setStatus(serverId, LinkState.REENROLL, server.reenrollReason)
                waitFor(Long.MAX_VALUE)
                continue
            }
            // While offline, retries keep the red "Offline" until one succeeds (no flicker).
            val st = Registry.status(serverId)
            if (failures == 0 || st.state != LinkState.DISCONNECTED) {
                Registry.setStatus(serverId, LinkState.CONNECTING, st.detail.takeIf { st.state == LinkState.CONNECTING })
            }
            val delay = try {
                runSession(server)
            } catch (e: InterruptedException) {
                break
            } catch (e: Exception) {
                Log.w(TAG, "session $serverId failed", e)
                Registry.setStatus(serverId, LinkState.DISCONNECTED, e.message ?: e.javaClass.simpleName)
                backoff()
            } finally {
                connected = false
                socket?.cancel()
                socket = null
                if (session != 0L) {
                    Native.sessionFree(session)
                    session = 0L
                }
            }
            if (running && delay > 0) waitFor(delay)
        }
        connected = false
    }

    private fun backoff(): Long {
        failures = minOf(failures + 1, 16)
        val base = minOf(MAX_BACKOFF_MS, 1000L shl minOf(failures - 1, 5))
        return base / 2 + Random.nextLong(base / 2 + 1)
    }

    /** Sleeps until the delay elapses, the network comes back, or we are stopped. */
    private fun waitFor(ms: Long) {
        val deadline = if (ms == Long.MAX_VALUE) Long.MAX_VALUE else System.currentTimeMillis() + ms
        while (running) {
            val left = deadline - System.currentTimeMillis()
            if (left <= 0) return
            when (events.poll(minOf(left, 60_000L), TimeUnit.MILLISECONDS)) {
                Event.Wake, Event.Stop -> return
                else -> if (deadline == Long.MAX_VALUE && store.server(serverId)?.reenrollReason == null) return
            }
        }
    }

    private inner class Listener : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            events.offer(Event.Open)
        }

        override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
            events.offer(Event.Binary(bytes.toByteArray()))
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            events.offer(Event.Text(text))
        }

        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
            events.offer(Event.Closed(code, reason))
            webSocket.close(1000, null)
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            events.offer(Event.Closed(code, reason))
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            events.offer(Event.Failure(t))
        }
    }

    private class SessionEnd(val retryAfterMs: Long, message: String) : Exception(message)

    /** Runs one session. Returns how long to wait before reconnecting. */
    private fun runSession(server: app.inbox.data.Server): Long {
        events.clear()
        pingSentAt.clear()
        val address = ServerAddress.parse(server.address) ?: run {
            store.setReenroll(serverId, "Invalid address")
            return 0
        }
        val ws = Http.client.newWebSocket(Request.Builder().url(address.ws("/v1/session")).build(), Listener())
        socket = ws
        // Set once the server welcomed us: losing such a session first shows
        // "Reconnecting…"; "Offline" waits for a reconnect attempt to fail.
        var welcomed = false

        try {
            // Handshake.
            expectOpen()
            val hello = JSONObject()
                .put("protocol", Native.protocolVersion())
                .put("device_id", server.deviceId)
                .put("app_version", BuildConfig.VERSION_NAME)
            val key = KeyVault.open(server.deviceKeySealed)
            val started = try {
                Native.sessionStart(key, server.serverPublicKey, hello.toString().toByteArray())
            } finally {
                key.fill(0)
            }
            session = ByteBuffer.wrap(started, 0, 8).long
            ws.send(started.copyOfRange(8, started.size).toByteString())
            val msg2 = expectBinary()
            val accept = JSONObject(String(Native.sessionFinish(session, msg2)))
            Log.i(TAG, "session established with ${accept.optString("server_name")}")
            send(JSONObject().put("t", "ready"))

            // Established: wait for welcome, then serve.
            var heartbeat = server.heartbeatMs
            var lastFrame = System.currentTimeMillis()
            var lastPing = lastFrame
            while (running) {
                val now = System.currentTimeMillis()
                val silence = noAnswerTimeout(heartbeat)
                if (now - lastFrame > silence) throw SessionEnd(1_000, "No answer from the server")
                if (welcomed && now - lastPing >= heartbeat) {
                    ping()
                    lastPing = now
                }
                val wait = maxOf(100L, minOf(heartbeat - (now - lastPing), silence - (now - lastFrame)))
                when (val e = events.poll(wait, TimeUnit.MILLISECONDS)) {
                    null -> {}
                    is Event.Binary -> {
                        lastFrame = System.currentTimeMillis()
                        Registry.frame(serverId, lastFrame)
                        val msg = JSONObject(String(Native.sessionDecrypt(session, e.bytes)))
                        when (msg.getString("t")) {
                            "welcome" -> {
                                heartbeat = msg.getLong("heartbeat_interval_ms").coerceAtLeast(5_000L)
                                val reconcile = onWelcome(msg)
                                welcomed = true
                                connected = true
                                failures = 0
                                Registry.setStatus(serverId, LinkState.CONNECTED, msg.optString("server_name"))
                                syncUserName(msg.optNullableString("user_name"))
                                for (g in reconcile.subscribe) send(JSONObject().put("t", "subscribe").put("group", g))
                                for (g in reconcile.unsubscribe) send(JSONObject().put("t", "unsubscribe").put("group", g))
                                ping()
                            }
                            "alert" -> onAlert(msg)
                            "subscriptions" -> Registry.changed()
                            "pong" -> onPong(msg.getLong("nonce"))
                            "user_name" -> Registry.changed()
                            "error" -> Log.w(TAG, "server error ${msg.optString("code")}: ${msg.optString("message")}")
                            else -> Log.w(TAG, "unknown message ${msg.getString("t")}")
                        }
                    }
                    is Event.Command -> if (connected) send(e.json)
                    is Event.Closed -> return onClosed(e.code, e.reason, welcomed)
                    is Event.Failure -> throw SessionEnd(backoff(), e.error.message ?: "Connection lost")
                    Event.Stop -> return 0
                    Event.Wake, Event.Open -> {}
                    is Event.Text -> throw SessionEnd(backoff(), "Unexpected answer from the server")
                }
            }
            return 0
        } catch (e: SessionEnd) {
            if (welcomed) Registry.setStatus(serverId, LinkState.CONNECTING, context.getString(R.string.status_reconnecting))
            else Registry.setStatus(serverId, LinkState.DISCONNECTED, e.message)
            return e.retryAfterMs
        } finally {
            ws.close(1000, null)
        }
    }

    private fun expectOpen() {
        val deadline = System.currentTimeMillis() + HANDSHAKE_TIMEOUT_MS
        while (true) {
            when (val e = events.poll(maxOf(1, deadline - System.currentTimeMillis()), TimeUnit.MILLISECONDS)) {
                Event.Open -> return
                null -> throw SessionEnd(backoff(), "Server unreachable")
                is Event.Failure -> throw SessionEnd(backoff(), "Server unreachable: ${e.error.message}")
                is Event.Closed -> throw SessionEnd(onClosed(e.code, e.reason), "Connection refused")
                Event.Stop -> throw InterruptedException()
                else -> {}
            }
        }
    }

    private fun expectBinary(): ByteArray {
        val deadline = System.currentTimeMillis() + HANDSHAKE_TIMEOUT_MS
        while (true) {
            when (val e = events.poll(maxOf(1, deadline - System.currentTimeMillis()), TimeUnit.MILLISECONDS)) {
                is Event.Binary -> return e.bytes
                null -> throw SessionEnd(backoff(), "The server does not answer")
                is Event.Failure -> throw SessionEnd(backoff(), "Connection lost: ${e.error.message}")
                is Event.Closed -> throw SessionEnd(onClosed(e.code, e.reason), "Connection refused")
                Event.Stop -> throw InterruptedException()
                else -> {}
            }
        }
    }

    /**
     * Silence after which the session is considered dead: 2.5 heartbeats, but at
     * least 4 measured round trips and 10 s, so a slow link is not cut needlessly.
     */
    private fun noAnswerTimeout(heartbeat: Long): Long {
        val rtt = Registry.liveness(serverId).rttMs ?: 0L
        return maxOf(heartbeat * 5 / 2, rtt * 4, 10_000L)
    }

    /**
     * Maps a close code to the reconnect policy. Returns the delay before reconnecting.
     * [wasConnected]: the session had been welcomed, so an ordinary close shows
     * "Reconnecting…" until the next attempt says otherwise.
     */
    private fun onClosed(code: Int, reason: String, wasConnected: Boolean = false): Long {
        connected = false
        return when (code) {
            CLOSE_UNKNOWN_DEVICE, CLOSE_REVOKED -> {
                val why = if (code == CLOSE_REVOKED) "Device revoked by the administrator" else "Device unknown to the server"
                store.setReenroll(serverId, "$why: delete this server and add it again")
                Registry.setStatus(serverId, LinkState.REENROLL, why)
                0
            }
            CLOSE_PROTOCOL -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Incompatible version: update the app")
                MAX_BACKOFF_MS * 10
            }
            CLOSE_RATE_LIMITED -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Too many attempts, retrying in a minute")
                60_000L
            }
            CLOSE_REPLACED -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Session replaced")
                5_000L
            }
            CLOSE_RESTART -> {
                if (wasConnected) Registry.setStatus(serverId, LinkState.CONNECTING, context.getString(R.string.status_reconnecting))
                else Registry.setStatus(serverId, LinkState.DISCONNECTED, "Server restarting")
                1_000L + Random.nextLong(2_000L)
            }
            else -> {
                if (wasConnected) Registry.setStatus(serverId, LinkState.CONNECTING, context.getString(R.string.status_reconnecting))
                else Registry.setStatus(serverId, LinkState.DISCONNECTED, "Disconnected ($code${if (reason.isNotEmpty()) " $reason" else ""})")
                backoff()
            }
        }
    }

    private fun send(msg: JSONObject) {
        val ws = socket ?: throw SessionEnd(backoff(), "Connection lost")
        val frame = Native.sessionEncrypt(session, msg.toString().toByteArray())
        if (!ws.send(frame.toByteString())) throw SessionEnd(backoff(), "Connection lost")
    }

    /** Heartbeat; its pong gives the round trip shown on the server screen. */
    private fun ping() {
        val n = ++nonce
        pingSentAt[n] = System.currentTimeMillis()
        if (pingSentAt.size > 64) pingSentAt.remove(pingSentAt.firstKey())
        send(JSONObject().put("t", "ping").put("nonce", n))
    }

    // ------------------------------------------------------------ messages

    /** Protocol v2: the server's subscriptions are group ids only. */
    private fun parseSubscriptions(arr: JSONArray): Set<String> = buildSet {
        for (i in 0 until arr.length()) add(arr.getString(i))
    }

    /** The username set on this phone for this server wins over what the server recorded. */
    private fun syncUserName(recorded: String?) {
        val local = store.server(serverId)?.userName ?: return
        if (local != recorded) send(JSONObject().put("t", "set_user_name").put("user_name", local))
    }

    private fun onWelcome(msg: JSONObject): Store.Reconcile {
        val groups = msg.getJSONArray("groups")
        val list = (0 until groups.length()).map {
            val g = groups.getJSONObject(it)
            Group(g.getString("id"), g.getString("name"), g.getString("description"))
        }
        return store.saveWelcome(
            serverId,
            msg.getString("server_name"),
            list,
            parseSubscriptions(msg.getJSONArray("subscriptions")),
            msg.getLong("heartbeat_interval_ms"),
            msg.getLong("offline_warning_after_ms"),
        )
    }

    private fun onAlert(msg: JSONObject) {
        val alert = Alert(
            serverId = serverId,
            alertId = msg.getString("alert_id"),
            group = msg.getString("group"),
            groupName = msg.getString("group_name"),
            title = msg.getString("title"),
            createdAtMs = msg.getLong("created_at_ms"),
            receivedAtMs = System.currentTimeMillis(),
            action = localAction(msg.getString("group")),
            notified = false,
            stoppedAtMs = null,
            readAtMs = null,
        )
        // 1. Durable before acknowledging: if we die after this, it is shown at restart.
        val isNew = store.insertAlertIfNew(alert)
        // 2. Stop server retransmission (also for duplicates whose first ack was lost).
        send(JSONObject().put("t", "ack").put("alert_id", alert.alertId).put("stage", "received"))
        if (!isNew) return
        // 3. Show it exactly once.
        Alerter.present(context, alert)
        store.markNotified(serverId, alert.alertId)
        Registry.changed()
    }

    /**
     * The level is this phone's choice. An alert of a group just turned off was
     * already targeted by the server: it is still shown, as a plain notification.
     */
    private fun localAction(group: String): String =
        store.levels(serverId)[group]?.takeIf { it == "alarm" || it == "ring" || it == "notify" } ?: Store.LEVEL_NOTIFY

    private fun JSONObject.optNullableString(key: String): String? =
        if (!has(key) || isNull(key)) null else getString(key).ifEmpty { null }

    private fun onPong(n: Long) {
        val now = System.currentTimeMillis()
        Registry.pong(serverId, now, pingSentAt.remove(n)?.let { now - it })
    }
}
