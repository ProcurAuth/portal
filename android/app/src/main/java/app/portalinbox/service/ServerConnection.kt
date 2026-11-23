package app.portalinbox.service

import android.content.Context
import android.util.Log
import app.portalinbox.BuildConfig
import app.portalinbox.R
import app.portalinbox.alert.Alerter
import app.portalinbox.core.Native
import app.portalinbox.data.Alert
import app.portalinbox.data.Group
import app.portalinbox.alert.ItemAlerter
import app.portalinbox.data.ItemEvent
import app.portalinbox.data.PrefsSync
import app.portalinbox.data.Kind
import app.portalinbox.data.KeyVault
import app.portalinbox.data.Levels
import app.portalinbox.data.OutboxEntry
import app.portalinbox.data.Reconcile
import app.portalinbox.data.Store
import app.portalinbox.net.Http
import app.portalinbox.net.ServerAddress
import app.portalinbox.proto.AgentFrame
import app.portalinbox.proto.AgentResultFrame
import app.portalinbox.proto.AckFrame
import app.portalinbox.proto.AnswerResultFrame
import app.portalinbox.proto.ClientFrame
import app.portalinbox.proto.ClientFrames
import app.portalinbox.proto.DigestFrame
import app.portalinbox.proto.SetPrefsFrame
import app.portalinbox.proto.ErrorFrame
import app.portalinbox.proto.FrameException
import app.portalinbox.proto.MessageFrame
import app.portalinbox.proto.PingFrame
import app.portalinbox.proto.PongFrame
import app.portalinbox.proto.ReadyFrame
import app.portalinbox.proto.RequestFrame
import app.portalinbox.proto.ServerAlertFrame
import app.portalinbox.proto.ServerFrame
import app.portalinbox.proto.ServerFrames
import app.portalinbox.proto.SettledFrame
import app.portalinbox.proto.SubscribeFrame
import app.portalinbox.proto.SubscriptionsFrame
import app.portalinbox.proto.UnsubscribeFrame
import app.portalinbox.proto.WelcomeFrame
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
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
 *
 * Items (protocol 5): the `ItemEngine` writes each `request`, `message`, `settled` and `digest`
 * frame in one transaction. Only then does this class send the receipt `ack` with the `id` of the
 * delivery. At each `welcome` it sends `sync` and the outbox. A frame of the server that breaks a
 * rule is logged without its content and dropped: it is never acknowledged.
 */
class ServerConnection(private val context: Context, val serverId: Long) {
    private sealed interface Event {
        object Open : Event
        class Binary(val bytes: ByteArray) : Event
        class Text(val text: String) : Event
        class Closed(val code: Int, val reason: String) : Event
        class Failure(val error: Throwable) : Event
        class Command(val frame: ClientFrame) : Event
        object Wake : Event
        object Flush : Event
        object Stop : Event
    }

    companion object {
        private const val TAG = "Inbox.Connection"
        private const val HANDSHAKE_TIMEOUT_MS = 15_000L
        /** Wait after close 4426 (the app is too old for the portal): long, as a retry cannot help before an update. */
        private const val INCOMPATIBLE_RETRY_MS = 300_000L
        private const val SUBSCRIBE_SETTLE_MS = 3_000L

        const val CLOSE_UNKNOWN_DEVICE = 4401
        const val CLOSE_REVOKED = 4403
        const val CLOSE_REPLACED = 4409
        const val CLOSE_PROTOCOL = 4426
        const val CLOSE_RATE_LIMITED = 4429
        const val CLOSE_RESTART = 1012
    }

    private val store = Store.get(context)
    private val engine = Items.engine(context)
    private val events = LinkedBlockingQueue<Event>()
    @Volatile private var running = true
    @Volatile private var connected = false
    private var socket: WebSocket? = null
    private var session = 0L
    private val backoff = Backoff()
    private var nonce = 0L
    private val pingSentAt = sortedMapOf<Long, Long>()

    /** True while the server of this session handles items (`welcome` with a purge age, no `not_enabled`). */
    @Volatile private var itemsEnabled = false

    /** The outbox entries that this session already sent. */
    private val outboxSent = HashSet<String>()

    /** When this session last asked the server to subscribe to each group. */
    private val subscribeSentAt = HashMap<String, Long>()
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

    /**
     * Sends the frames of the outbox that this session did not send yet. Call it after
     * `ItemEngine.approve`, `decline`, `reply` or `agentDo`. Offline, the frames wait for the next `welcome`.
     */
    fun flushOutbox() {
        events.offer(Event.Flush)
    }

    /**
     * Sends the preferences of the account (`set_prefs`). The caller saved them as pending ([PrefsSync.edit]). Offline, the
     * next `welcome` sends them. The server sends no answer: it confirms with the next `welcome`.
     */
    fun sendPrefs(frame: SetPrefsFrame) {
        if (itemsEnabled) events.offer(Event.Command(frame))
    }

    /**
     * Sets this phone's level for a group (null = off). The level stays on the phone:
     * the server only learns whether the group is subscribed. Offline changes are
     * applied by the next Welcome reconciliation.
     */
    fun setLevel(group: String, action: String?) {
        store.setLevel(serverId, group, action)
        // The pseudo group of the alerts to the account is not on the server: nothing to send.
        val frame = if (action == null) UnsubscribeFrame(group) else SubscribeFrame(group)
        if (connected && Levels.isServerGroup(group)) events.offer(Event.Command(frame))
        Registry.changed()
    }

    // ------------------------------------------------------------ main loop

    private fun loop() {
        while (running) {
            val server = store.server(serverId) ?: break
            if (server.reenrollReason != null) {
                Registry.setStatus(
                    serverId, KeyChange.stateOf(server.reenrollReason) ?: LinkState.REENROLL, KeyChange.detail(server.reenrollReason),
                )
                waitFor(Long.MAX_VALUE)
                continue
            }
            // While offline, retries keep the red "Offline" until one succeeds (no flicker).
            val st = Registry.status(serverId)
            if (backoff.failures == 0 || st.state != LinkState.DISCONNECTED) {
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
                // The last frame time, so "last sync" is exact after a loss. It never moves the value back.
                Registry.liveness(serverId).lastFrameMs?.let { store.touchSync(serverId, it, force = true) }
                if (session != 0L) {
                    Native.sessionFree(session)
                    session = 0L
                }
            }
            if (running && delay > 0) waitFor(delay)
        }
        connected = false
    }

    /** Counts a failure and gives the delay before the next try: see [Backoff]. */
    private fun backoff(): Long = backoff.next()

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

    /** The handshake did not end with the pinned key. */
    private class KeyMismatch : Exception("The portal does not prove the pinned key")

    /** Runs one session. Returns how long to wait before reconnecting. */
    private fun runSession(server: app.portalinbox.data.Server): Long {
        events.clear()
        pingSentAt.clear()
        outboxSent.clear()
        itemsEnabled = false
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
            val hello = org.json.JSONObject()
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
            val accept = try {
                org.json.JSONObject(String(Native.sessionFinish(session, msg2)))
            } catch (e: IllegalStateException) {
                // Message 2 does not authenticate with the pinned key: this is not the portal that was enrolled (or its key changed).
                // The connection stops here. Any other native failure goes up as an ordinary failure.
                if (KeyChange.isHandshakeKeyFailure(e.message)) throw KeyMismatch()
                throw e
            }
            Log.i(TAG, "session established with ${accept.optString("server_name")}")
            send(ReadyFrame)

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
                        store.touchSync(serverId, lastFrame)
                        val frame = decode(Native.sessionDecrypt(session, e.bytes))
                        when (frame) {
                            null -> {}
                            is WelcomeFrame -> {
                                heartbeat = frame.heartbeatIntervalMs.coerceAtLeast(5_000L)
                                val reconcile = onWelcome(frame)
                                store.touchSync(serverId, lastFrame, force = true)
                                welcomed = true
                                connected = true
                                backoff.reset()
                                Registry.setStatus(serverId, LinkState.CONNECTED, frame.serverName)
                                for (g in reconcile.subscribe) send(SubscribeFrame(g))
                                for (g in reconcile.unsubscribe) send(UnsubscribeFrame(g))
                                // The items: tell the server which ones are open, then send the outbox again.
                                ServerClock.update(context, serverId, frame.serverTimeMs)
                                val start = engine.onWelcome(serverId, frame)
                                itemsEnabled = frame.itemsEnabled
                                start.sync?.let { send(it) }
                                flush(start.outbox)
                                // The preferences: the values of the server, or the edit of the owner that it lacks.
                                when (val action = PrefsSync.onWelcome(PendingPrefsStore.get(context), serverId, frame)) {
                                    is PrefsSync.Action.Send -> send(action.frame)
                                    PrefsSync.Action.None -> {}
                                }
                                Registry.changed()
                                ping()
                            }
                            is ServerAlertFrame -> onAlert(frame)
                            is SubscriptionsFrame -> onSubscriptions(frame)
                            is PongFrame -> onPong(frame.nonce)
                            is ErrorFrame -> {
                                Log.w(TAG, "server error ${frame.code}: ${frame.message}")
                                if (frame.isNotEnabled) itemsEnabled = false
                                onPrefsError(frame)
                            }
                            // The agents: the engine keeps an `agent` frame by `rev`, the receipt goes after the write, and an
                            // `agent_result` closes the entry of the outbox and gives a line for the snackbar.
                            is RequestFrame, is MessageFrame, is SettledFrame, is DigestFrame, is AnswerResultFrame,
                            is AgentFrame, is AgentResultFrame -> onItemFrame(frame)
                        }
                    }
                    is Event.Command -> if (connected) send(e.frame)
                    Event.Flush -> if (connected && itemsEnabled) flush(engine.pendingOutbox(serverId))
                    is Event.Closed -> return onClosed(e.code, e.reason, welcomed)
                    is Event.Failure -> throw SessionEnd(backoff(), e.error.message ?: "Connection lost")
                    Event.Stop -> return 0
                    Event.Wake, Event.Open -> {}
                    is Event.Text -> throw SessionEnd(backoff(), "Unexpected answer from the server")
                }
            }
            return 0
        } catch (e: KeyMismatch) {
            keyChanged(context.getString(R.string.key_changed_handshake))
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
            CLOSE_UNKNOWN_DEVICE -> {
                keyChanged(context.getString(R.string.key_changed_closed))
                0
            }
            CLOSE_REVOKED -> {
                val why = "Device revoked by the administrator"
                store.setReenroll(serverId, "$why: delete this server and add it again")
                Registry.setStatus(serverId, LinkState.REENROLL, why)
                0
            }
            CLOSE_PROTOCOL -> {
                Registry.setStatus(serverId, LinkState.DISCONNECTED, "Incompatible version: update the app")
                INCOMPATIBLE_RETRY_MS
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

    /** Stops the connection: the key of this portal is not the pinned one. The user fixes it from the screen of the portal. */
    private fun keyChanged(detail: String) {
        store.setReenroll(serverId, KeyChange.reason(detail))
        Registry.setStatus(serverId, LinkState.KEY_CHANGED, detail)
    }

    private fun send(frame: ClientFrame) {
        val ws = socket ?: throw SessionEnd(backoff(), "Connection lost")
        if (frame is SubscribeFrame) subscribeSentAt[frame.group] = System.currentTimeMillis()
        val sealed = Native.sessionEncrypt(session, ClientFrames.encode(frame))
        if (!ws.send(sealed.toByteString())) throw SessionEnd(backoff(), "Connection lost")
    }

    /** Heartbeat; its pong gives the round trip shown on the server screen. */
    private fun ping() {
        val n = ++nonce
        pingSentAt[n] = System.currentTimeMillis()
        if (pingSentAt.size > 64) pingSentAt.remove(pingSentAt.firstKey())
        send(PingFrame(n))
    }

    /** Sends the entries that this session did not send, oldest first. Each one waits for its `answer_result`. */
    private fun flush(entries: List<OutboxEntry>) {
        for (entry in entries) {
            if (!outboxSent.add(entry.clientMsgId)) continue
            val frame = try {
                entry.toFrame()
            } catch (e: FrameException) {
                Log.w(TAG, "outbox entry ${entry.clientMsgId} is not a valid frame: ${e.message}")
                continue
            }
            send(frame)
            engine.markSent(entry)
        }
    }

    /** Reads a frame of the server. A frame that breaks a rule is dropped and never acknowledged. */
    private fun decode(plain: ByteArray): ServerFrame? = try {
        ServerFrames.decode(plain)
    } catch (e: FrameException) {
        Log.w(TAG, "refused a frame of ${plain.size} bytes: ${e.message}")
        null
    }

    // ------------------------------------------------------------ messages

    private fun onWelcome(f: WelcomeFrame): Reconcile = store.saveWelcome(
        serverId,
        f.serverName,
        f.userName.ifEmpty { null },
        f.groups.map { Group(it.id, it.name, it.description) },
        f.subscriptions.toSet(),
        f.heartbeatIntervalMs,
        f.offlineWarningAfterMs,
    )

    /**
     * The server sends its subscriptions after each change, also when the account lost the
     * permission for a group. A group that the server no longer has is turned off here, so the
     * screen tells the truth. A group that this session asked for a moment ago is left alone: the
     * message can be older than the request.
     */
    private fun onSubscriptions(frame: SubscriptionsFrame) {
        val server = frame.subscriptions.toSet()
        val now = System.currentTimeMillis()
        for (group in Levels.lostOnServer(store.levels(serverId), server, subscribeSentAt, now, SUBSCRIBE_SETTLE_MS)) {
            store.setLevel(serverId, group, null)
        }
        Registry.changed()
    }

    private fun onAlert(frame: ServerAlertFrame) {
        val alert = Alert(
            serverId = serverId,
            alertId = frame.alertId,
            group = frame.group,
            groupName = frame.groupName,
            kind = Kind.ALERT,
            title = frame.title,
            createdAtMs = frame.createdAtMs,
            receivedAtMs = System.currentTimeMillis(),
            action = localAction(frame.group),
            notified = false,
            stoppedAtMs = null,
            readAtMs = null,
        )
        // 1. Durable before acknowledging: if we die after this, it is shown at restart.
        val isNew = store.insertAlertIfNew(alert)
        // 2. Stop server retransmission (also for duplicates whose first ack was lost).
        send(AckFrame(alert.alertId))
        if (!isNew) return
        // 3. Show it exactly once.
        Alerter.present(context, alert)
        store.markNotified(serverId, alert.alertId)
        Registry.changed()
    }

    /**
     * The level is this phone's choice: see [Levels.alertAction]. An alert of a group just turned
     * off was already targeted by the server: it is still shown, as a plain notification. An
     * alert to the account (`@direct`) uses its own level, `ring` by default.
     */
    private fun localAction(group: String): String = Levels.alertAction(group, store.levels(serverId))

    /**
     * A frame about an item. The engine stores it durably in one transaction. The receipt goes
     * out after that. A failure of the storage ends the session: the server sends the row again.
     */
    private fun onItemFrame(frame: ServerFrame) {
        val outcome = engine.onFrame(serverId, frame)
        outcome.ack?.let { send(AckFrame(it)) }
        for (event in outcome.events) present(event)
        if (outcome.events.isNotEmpty()) Registry.changed()
    }

    /**
     * The place where the notifications of the items hook in. A new request notifies and the phone then marks the item as
     * notified. A settled item withdraws its notification. A turn of the agent notifies on the channel of the replies. A
     * digest makes the quiet summary. A request that comes back says "The agent is back".
     */
    private fun present(event: ItemEvent) {
        Log.d(TAG, "item event ${event.javaClass.simpleName}")
        ItemAlerter.onEvent(context, event)
    }

    /**
     * The portal refused the last `set_prefs` (`invalid_prefs`): it stored nothing. The pending edit goes away, so it is not
     * sent again at each `welcome`, and the screen of the settings says that the portal refused it.
     */
    private fun onPrefsError(frame: ErrorFrame) {
        val refusal = PrefsSync.onError(PendingPrefsStore.get(context), serverId, frame.code, frame.message) ?: return
        Registry.setRefusal(serverId, refusal.message)
    }

    private fun onPong(n: Long) {
        val now = System.currentTimeMillis()
        Registry.pong(serverId, now, pingSentAt.remove(n)?.let { now - it })
    }
}
