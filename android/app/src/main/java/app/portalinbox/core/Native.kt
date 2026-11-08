package app.portalinbox.core

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
     * Reads an enrollment code (text or `portal-inbox://enroll?...` link).
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

    /**
     * The effect hash of an operation (ProcurAuth 17.3). `opJson` is the JSON text of `op`.
     * Returns `{"effect": "sha256:..."}` or `{"error": {"code": "invalid_op", "message"}}`.
     * The parser refuses a float, an integer above 2^53-1 and a duplicate member name.
     */
    @JvmStatic external fun effectCompute(service: String, action: String, resource: String, opJson: String): String

    /**
     * The view of an approval with the values of `op`. Returns `{"view": {"title",
     * "fields": [{"label", "value"}], "body", "body_label"?}}` or
     * `{"error": {"code": "invalid_op" | "invalid_view", "message"}}`.
     */
    @JvmStatic external fun viewResolve(opJson: String, viewJson: String): String

    /**
     * The tree of a Markdown text (the safe subset of `inbox_core::render::markdown`), as JSON text. It never
     * fails. Blocks: `paragraph`, `heading` (`level`), `code` (`lang`, `text`), `quote`, `list` (`ordered`,
     * `start`, `items`), `text`. Inline nodes: `text`, `code`, `em`, `strong`, `link` (`host`), `br`, `soft`.
     * Over a limit the tree has one `text` block and `"fallback": true`.
     */
    @JvmStatic external fun markdownParse(text: String): String

    /**
     * The color of a code text: a JSON array `[[start, end, "class"], ...]`. The offsets are UTF-16 units, as
     * Kotlin indexes a string. The tokens cover the whole text. A language that is not in the closed list gives
     * one `plain` token.
     */
    @JvmStatic external fun highlightTokens(language: String, text: String): String

    @JvmStatic external fun protocolVersion(): Int
}
