package app.inbox.core

/**
 * JNI bindings to `inbox-core` (crates/inbox-android).
 *
 * All Inbox cryptography (OPAQUE enrollment, Noise sessions) runs in Rust; this
 * object only moves bytes. Handles must be freed exactly once.
 */
object Native {
    init {
        System.loadLibrary("inbox_android")
    }

    /**
     * Starts an enrollment announcing `userName` (the pseudo; empty for none).
     * Returns `{"handle": Long, "send": String}` or `{"error": {"code", "message"}}`.
     */
    @JvmStatic external fun enrollStart(password: ByteArray, deviceName: String, userName: String): String

    /** Returns `{"send": String}`, `{"done": {...}}` or `{"error": {...}}`. */
    @JvmStatic external fun enrollOnMessage(handle: Long, text: String): String

    @JvmStatic external fun enrollFree(handle: Long)

    /** Returns an 8-byte big-endian handle followed by Noise message 1. */
    @JvmStatic external fun sessionStart(devicePrivate: ByteArray, serverPublic: ByteArray, hello: ByteArray): ByteArray

    /** Consumes Noise message 2 and returns the accept payload. Throws if the server key is wrong. */
    @JvmStatic external fun sessionFinish(handle: Long, msg2: ByteArray): ByteArray

    @JvmStatic external fun sessionEncrypt(handle: Long, plaintext: ByteArray): ByteArray

    /** Throws on authentication failure; the session is then unusable. */
    @JvmStatic external fun sessionDecrypt(handle: Long, frame: ByteArray): ByteArray

    @JvmStatic external fun sessionFree(handle: Long)

    @JvmStatic external fun protocolVersion(): Int
}
