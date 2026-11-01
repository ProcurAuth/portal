package app.inbox.net

import android.content.Context
import app.inbox.BuildConfig
import app.inbox.R
import app.inbox.core.Native
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import org.json.JSONObject
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/** A valid enrollment code: where to connect and which server key to trust. */
class EnrollCodeInfo(
    val address: ServerAddress,
    val serverPublicKey: ByteArray,
    /** First 8 hexadecimal characters of the SHA-256 of the server key, to compare with the administrator. */
    val fingerprint: String,
)

/** Result of a successful enrollment. */
class Enrollment(
    val address: ServerAddress,
    val serverName: String,
    val deviceId: String,
    /** Login of the account on the server. */
    val userName: String,
    val devicePrivateKey: ByteArray,
    val serverPublicKey: ByteArray,
)

/** Which form field an enrollment error is about (the screen shows it under that field). */
enum class EnrollField { CODE, DEVICE_NAME, NONE }

class EnrollmentError(message: String, val field: EnrollField = EnrollField.NONE) : Exception(message)

/**
 * Enrolls this phone with a one-time enrollment code (protocol 4). The code holds the
 * server address, the server key to trust and a one-time secret. Blocking: call from a
 * worker thread, except [parse].
 */
object Enroller {
    // Close codes of `/v1/enroll`.
    private const val CLOSE_ENROLL_REFUSED = 4410
    private const val CLOSE_PROTOCOL = 4426
    private const val CLOSE_RATE_LIMITED = 4429

    private sealed interface Ev {
        object Open : Ev
        class Binary(val bytes: ByteArray) : Ev
        class Closed(val code: Int) : Ev
        class Failure(val t: Throwable, val http: Int?) : Ev
    }

    /** Reads the text typed or the link tapped. Throws [EnrollmentError] on the field CODE. */
    fun parse(context: Context, text: String): EnrollCodeInfo {
        val out = JSONObject(Native.enrollParse(text))
        out.optJSONObject("error")?.let { throw describe(context, it) }
        val code = out.getJSONObject("code")
        val address = ServerAddress.parse(code.getString("address"))
            ?: throw EnrollmentError(context.getString(R.string.err_code_invalid), EnrollField.CODE)
        return EnrollCodeInfo(
            address,
            android.util.Base64.decode(code.getString("server_public_key"), android.util.Base64.NO_WRAP),
            code.getString("fingerprint"),
        )
    }

    fun enroll(context: Context, codeText: String, deviceName: String): Enrollment {
        val info = parse(context, codeText)
        val q = LinkedBlockingQueue<Ev>()
        var handle = 0L
        var ws: WebSocket? = null
        try {
            val start = JSONObject(Native.enrollStart(codeText, deviceName, BuildConfig.VERSION_NAME))
            start.optJSONObject("error")?.let { throw describe(context, it) }
            handle = start.getLong("handle")
            val msg1 = android.util.Base64.decode(start.getString("send"), android.util.Base64.NO_WRAP)
            ws = Http.client.newWebSocket(
                Request.Builder().url(info.address.ws("/v1/enroll")).build(),
                object : WebSocketListener() {
                    override fun onOpen(webSocket: WebSocket, response: Response) { q.offer(Ev.Open) }
                    override fun onMessage(webSocket: WebSocket, bytes: ByteString) { q.offer(Ev.Binary(bytes.toByteArray())) }
                    override fun onClosing(webSocket: WebSocket, code: Int, reason: String) { q.offer(Ev.Closed(code)) }
                    override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) { q.offer(Ev.Failure(t, response?.code)) }
                },
            )
            when (val e = q.poll(20, TimeUnit.SECONDS)) {
                Ev.Open -> {}
                is Ev.Closed -> throw closed(context, e.code)
                is Ev.Failure -> throw failed(context, e)
                // No WebSocket at all: wrong address, server down or no network.
                else -> throw EnrollmentError(context.getString(R.string.err_unreachable))
            }
            ws.send(msg1.toByteString())
            val msg2 = when (val e = q.poll(90, TimeUnit.SECONDS)) {
                is Ev.Binary -> e.bytes
                is Ev.Closed -> throw closed(context, e.code)
                is Ev.Failure -> throw failed(context, e)
                else -> throw EnrollmentError(context.getString(R.string.err_no_answer))
            }
            val out = JSONObject(Native.enrollFinish(handle, msg2))
            out.optJSONObject("error")?.let { throw describe(context, it) }
            val d = out.getJSONObject("done")
            return Enrollment(
                address = info.address,
                serverName = d.getString("server_name"),
                deviceId = d.getString("device_id"),
                userName = d.getString("user_name"),
                devicePrivateKey = android.util.Base64.decode(d.getString("device_private_key"), android.util.Base64.NO_WRAP),
                serverPublicKey = android.util.Base64.decode(d.getString("server_public_key"), android.util.Base64.NO_WRAP),
            )
        } finally {
            if (handle != 0L) Native.enrollFree(handle)
            ws?.close(1000, null)
        }
    }

    /** The server refused or closed without message 2. */
    private fun closed(c: Context, code: Int): EnrollmentError = when (code) {
        CLOSE_ENROLL_REFUSED -> EnrollmentError(c.getString(R.string.err_code_refused), EnrollField.CODE)
        CLOSE_PROTOCOL -> EnrollmentError(c.getString(R.string.err_update_app))
        CLOSE_RATE_LIMITED -> EnrollmentError(c.getString(R.string.err_rate_limited))
        else -> EnrollmentError(c.getString(R.string.err_closed, code))
    }

    private fun failed(c: Context, e: Ev.Failure): EnrollmentError =
        if (e.http == 429) EnrollmentError(c.getString(R.string.err_rate_limited))
        else EnrollmentError(c.getString(R.string.err_lost, e.t.message ?: ""))

    private fun describe(c: Context, err: JSONObject): EnrollmentError = when (err.optString("code")) {
        "bad_code" -> EnrollmentError(c.getString(R.string.err_code_invalid), EnrollField.CODE)
        "bad_device_name" -> EnrollmentError(c.getString(R.string.err_device_name), EnrollField.DEVICE_NAME)
        // Message 2 does not come from the server key of the code (or it was altered).
        "auth_failed" -> EnrollmentError(c.getString(R.string.err_server_key))
        "protocol" -> EnrollmentError(c.getString(R.string.err_protocol, err.optString("message")))
        "internal" -> EnrollmentError(c.getString(R.string.err_internal))
        else -> EnrollmentError(err.optString("message").ifEmpty { c.getString(R.string.err_unknown) })
    }
}
