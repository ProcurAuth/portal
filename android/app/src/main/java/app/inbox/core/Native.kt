package app.inbox.core

/**
 * JNI bindings to `inbox-core` (crates/inbox-android).
 *
 * All Inbox cryptography (enrollment code, Noise enrollment and sessions) runs in Rust; this
 * object only moves bytes. Handles must be freed exactly once.
 */
object Native {
    init {
        System.loadLibrary("inbox_android")
    }

    /**
     * Reads an enrollment code (text or `inbox://enroll?...` link).
     * Returns `{"code": {"address", "server_public_key" (base64), "fingerprint"}}` or
     * `{"error": {"code", "message"}}`. The secret of the code stays in Rust.
     */
    @JvmStatic external fun enrollParse(code: String): String

    /**
     * Starts an enrollment with a fresh device key.
     * Returns `{"handle": Long, "send": String}` (Noise message 1, standard base64)
     * or `{"error": {"code", "message"}}`.
     */
    @JvmStatic external fun enrollStart(code: String, deviceName: String, appVersion: String): String

    /**
     * Consumes Noise message 2 (one binary frame). Returns `{"done": {...}}` or
     * `{"error": {...}}`. `done` holds the device private key: seal it at once.
     */
    @JvmStatic external fun enrollFinish(handle: Long, msg2: ByteArray): String

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
