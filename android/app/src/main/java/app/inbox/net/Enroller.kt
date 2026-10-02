package app.inbox.net

import android.content.Context
import app.inbox.R
import app.inbox.core.Native
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/** Result of a successful enrollment. */
class Enrollment(
    val serverName: String,
    val deviceId: String,
    val devicePrivateKey: ByteArray,
    val serverPublicKey: ByteArray,
)

class EnrollmentError(message: String) : Exception(message)

/**
 * Enrolls this phone with the shared device password (OPAQUE: the password never
 * leaves the phone). Blocking: call from a worker thread.
 */
object Enroller {
    private sealed interface Ev {
        object Open : Ev
        class Text(val text: String) : Ev
        class Closed(val code: Int, val reason: String) : Ev
        class Failure(val t: Throwable) : Ev
    }

    fun enroll(context: Context, address: ServerAddress, password: ByteArray, deviceName: String, userName: String): Enrollment {
        val str = { id: Int -> context.getString(id) }
        val q = LinkedBlockingQueue<Ev>()
        val ws = Http.client.newWebSocket(
            Request.Builder().url(address.ws("/v1/enroll")).build(),
            object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) { q.offer(Ev.Open) }
                override fun onMessage(webSocket: WebSocket, text: String) { q.offer(Ev.Text(text)) }
                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) { q.offer(Ev.Closed(code, reason)) }
                override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) { q.offer(Ev.Failure(t)) }
            },
        )
        var handle = 0L
        try {
            when (val e = q.poll(20, TimeUnit.SECONDS)) {
                Ev.Open -> {}
                is Ev.Failure -> throw EnrollmentError(context.getString(R.string.err_unreachable_detail, e.t.message ?: e.t.javaClass.simpleName))
                else -> throw EnrollmentError(str(R.string.err_unreachable))
            }
            val start = JSONObject(Native.enrollStart(password, deviceName, userName))
            start.optJSONObject("error")?.let { throw EnrollmentError(describe(context, it)) }
            handle = start.getLong("handle")
            ws.send(start.getString("send"))
            while (true) {
                val text = when (val e = q.poll(90, TimeUnit.SECONDS)) {
                    is Ev.Text -> e.text
                    is Ev.Closed -> throw EnrollmentError(context.getString(R.string.err_closed, e.code))
                    is Ev.Failure -> throw EnrollmentError(context.getString(R.string.err_lost, e.t.message ?: ""))
                    else -> throw EnrollmentError(str(R.string.err_no_answer))
                }
                val out = JSONObject(Native.enrollOnMessage(handle, text))
                out.optJSONObject("error")?.let { throw EnrollmentError(describe(context, it)) }
                out.optString("send", "").takeIf { it.isNotEmpty() }?.let { ws.send(it) }
                out.optJSONObject("done")?.let { d ->
                    return Enrollment(
                        serverName = d.getString("server_name"),
                        deviceId = d.getString("device_id"),
                        devicePrivateKey = android.util.Base64.decode(d.getString("device_private_key"), android.util.Base64.NO_WRAP),
                        serverPublicKey = android.util.Base64.decode(d.getString("server_public_key"), android.util.Base64.NO_WRAP),
                    )
                }
            }
        } finally {
            if (handle != 0L) Native.enrollFree(handle)
            password.fill(0)
            ws.close(1000, null)
        }
    }

    private fun describe(c: Context, err: JSONObject): String = when (err.optString("code")) {
        // A wrong password is detected locally by OPAQUE (the server cannot tell).
        "auth_failed", "bad_password" -> c.getString(R.string.err_bad_password)
        "rate_limited" -> c.getString(R.string.err_rate_limited)
        "too_many_devices" -> c.getString(R.string.err_too_many_devices)
        "protocol" -> c.getString(R.string.err_protocol, err.optString("message"))
        "internal" -> c.getString(R.string.err_internal)
        "malformed" -> c.getString(R.string.err_malformed)
        else -> err.optString("message").ifEmpty { c.getString(R.string.err_unknown) }
    }
}
