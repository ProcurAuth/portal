package app.inbox.service

import android.content.Context
import android.util.Log
import app.inbox.BuildConfig
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
        object Flush : Event
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
    /** Outbox rows sent on this session, deleted once a later pong proves the server processed them. */
    private val outboxInFlight = mutableSetOf<Long>()
    /** Servers older than the pseudo feature reject `set_user_name`: only send it when Welcome has the field. */
    @Volatile private var supportsUserName = false
    private val confirmOnPong = sortedMapOf<Long, List<Long>>()
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

    fun flushOutbox() {
        events.offer(Event.Flush)
    }

    fun isConnected() = connected

    /** Returns false when offline: subscriptions can only change on a live session. */
    /** Asks the server to track this phone under `userName`; sent again after each Welcome if needed. */
    fun setUserName(userName: String) {
        if (connected && supportsUserName) events.offer(Event.Command(JSONObject().put("t", "set_user_name").put("user_name", userName)))
    }

    fun subscribe(group: String, action: String?): Boolean {
        if (!connected) return false
        val msg = if (action == null) {
            JSONObject().put("t", "unsubscribe").put("group", group)
        } else {
            JSONObject().put("t", "subscribe").put("group", group).put("action", action)
        }
        events.offer(Event.Command(msg))
        return true
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
            Registry.setStatus(serverId, LinkState.CONNECTING, null)
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
        outboxInFlight.clear()
        confirmOnPong.clear()
        val address = ServerAddress.parse(server.address) ?: run {
            store.setReenroll(serverId, "Adresse invalide")
            return 0
        }
        val ws = Http.client.newWebSocket(Request.Builder().url(address.ws("/v1/session")).build(), Listener())
        socket = ws

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
            var welcomed = false
            while (running) {
                val now = System.currentTimeMillis()
                if (now - lastFrame > heartbeat * 5 / 2) throw SessionEnd(1_000, "Pas de réponse du serveur")
                if (welcomed && now - lastPing >= heartbeat) {
                    send(JSONObject().put("t", "ping").put("nonce", ++nonce))
                    lastPing = now
                }
                val wait = maxOf(100L, minOf(heartbeat - (now - lastPing), heartbeat * 5 / 2 - (now - lastFrame)))
                when (val e = events.poll(wait, TimeUnit.MILLISECONDS)) {
                    null -> {}
                    is Event.Binary -> {
                        lastFrame = System.currentTimeMillis()
                        val msg = JSONObject(String(Native.sessionDecrypt(session, e.bytes)))
                        when (msg.getString("t")) {
                            "welcome" -> {
                                heartbeat = msg.getLong("heartbeat_interval_ms").coerceAtLeast(5_000L)
                                onWelcome(msg)
                                welcomed = true
                                connected = true
                                failures = 0
                                Registry.setStatus(serverId, LinkState.CONNECTED, msg.optString("server_name"))
                                supportsUserName = msg.has("user_name")
                                if (supportsUserName) {
                                    syncUserName(if (msg.isNull("user_name")) null else msg.optString("user_name", "").ifEmpty { null })
                                }
                                flushPendingAcks()
                            }
                            "alert" -> onAlert(msg)
                            "subscriptions" -> {
                                store.saveSubscriptions(serverId, parseSubscriptions(msg.getJSONArray("subscriptions")))
                                Registry.changed()
                            }
                            "pong" -> onPong(msg.getLong("nonce"))
                            "user_name" -> Registry.changed()
                            "error" -> Log.w(TAG, "server error ${msg.optString("code")}: ${msg.optString("message")}")
                            else -> Log.w(TAG, "unknown message ${msg.getString("t")}")
                        }
                    }
                    is Event.Command -> if (connected) send(e.json)
                    Event.Flush -> if (connected) flushPendingAcks()
                    is Event.Closed -> return onClosed(e.code, e.reason)
                    is Event.Failure -> throw SessionEnd(backoff(), e.error.message ?: "Connexion perdue")
                    Event.Stop -> return 0
                    Event.Wake, Event.Open -> {}
                    is Event.Text -> throw SessionEnd(backoff(), "Réponse inattendue du serveur")
                }
            }
            return 0
        } catch (e: SessionEnd) {
            Registry.setStatus(serverId, LinkState.DISCONNECTED, e.message)
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
                null -> throw SessionEnd(backoff(), "Serveur injoignable")
                is Event.Failure -> throw SessionEnd(backoff(), "Serveur injoignable : ${e.error.message}")
                is Event.Closed -> throw SessionEnd(onClosed(e.code, e.reason), "Connexion refusée")
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
                null -> throw SessionEnd(backoff(), "Le serveur ne répond pas")
                is Event.Failure -> throw SessionEnd(backoff(), "Connexion perdue : ${e.error.message}")
                is Event.Closed -> throw SessionEnd(onClosed(e.code, e.reason), "Connexion refusée")
                Event.Stop -> throw InterruptedException()
                else -> {}
            }
        }
    }

    /** Maps a close code to the reconnect policy. Returns the delay before reconnecting. */
    private fun onClosed(code: Int, reason: String): Long {
        connected = false
        return when (code) {
            CLOSE_UNKNOWN_DEVICE, CLOSE_REVOKED -> {
                val why = if (code == CLOSE_REVOKED) "Appareil révoqué par l'administrateur" else "Appareil inconnu du serveur"
                store.setReenroll(serverId, "$why : supprimez puis ajoutez à nouveau ce serveur")
                Registry.setStatus(serverId, LinkState.REENROLL, why)
                0
            }
            CLOSE_PROTOCOL -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Version incompatible : mettez l'application à jour")
                MAX_BACKOFF_MS * 10
            }
            CLOSE_RATE_LIMITED -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Trop de tentatives, nouvel essai dans une minute")
                60_000L
            }
            CLOSE_REPLACED -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Session remplacée")
                5_000L
            }
            CLOSE_RESTART -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Redémarrage du serveur")
                1_000L + Random.nextLong(2_000L)
            }
            else -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Déconnecté ($code${if (reason.isNotEmpty()) " $reason" else ""})")
                backoff()
            }
        }
    }

    private fun send(msg: JSONObject) {
        val ws = socket ?: throw SessionEnd(backoff(), "Connexion perdue")
        val frame = Native.sessionEncrypt(session, msg.toString().toByteArray())
        if (!ws.send(frame.toByteString())) throw SessionEnd(backoff(), "Connexion perdue")
    }

    // ------------------------------------------------------------ messages

    private fun parseSubscriptions(arr: JSONArray): Map<String, String> = buildMap {
        for (i in 0 until arr.length()) {
            val s = arr.getJSONObject(i)
            put(s.getString("group"), s.getString("action"))
        }
    }

    /** The local pseudo for this server wins: older enrollments have none on the server. */
    private fun syncUserName(recorded: String?) {
        val local = store.server(serverId)?.userName ?: return
        if (local != recorded) send(JSONObject().put("t", "set_user_name").put("user_name", local))
    }

    private fun onWelcome(msg: JSONObject) {
        val groups = msg.getJSONArray("groups")
        val list = (0 until groups.length()).map {
            val g = groups.getJSONObject(it)
            Group(g.getString("id"), g.getString("name"), g.getString("description"))
        }
        store.saveWelcome(
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
            title = if (msg.isNull("title")) null else msg.getString("title"),
            message = if (msg.isNull("message")) null else msg.getString("message"),
            createdAtMs = msg.getLong("created_at_ms"),
            receivedAtMs = System.currentTimeMillis(),
            action = msg.getString("action"),
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

    private fun flushPendingAcks() {
        val sent = mutableListOf<Long>()
        for (item in store.outbox(serverId)) {
            if (item.id in outboxInFlight) continue
            // Older versions queued "displayed"/"acknowledged": alerts are one-way now.
            if (item.stage != "received") {
                store.removeOutbox(item.id)
                continue
            }
            send(JSONObject().put("t", "ack").put("alert_id", item.alertId).put("stage", item.stage))
            outboxInFlight.add(item.id)
            sent.add(item.id)
        }
        if (sent.isEmpty()) return
        // The server handles frames in order: its pong proves the acks before it were stored.
        val n = ++nonce
        confirmOnPong[n] = sent
        send(JSONObject().put("t", "ping").put("nonce", n))
    }

    private fun onPong(n: Long) {
        val done = confirmOnPong.headMap(n + 1)
        for (ids in done.values) {
            for (id in ids) {
                store.removeOutbox(id)
                outboxInFlight.remove(id)
            }
        }
        done.clear()
    }
}
