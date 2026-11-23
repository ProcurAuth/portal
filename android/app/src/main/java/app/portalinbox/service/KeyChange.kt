package app.portalinbox.service

/**
 * The "key changed" state of a portal. Pure Kotlin.
 *
 * A portal is in this state when the Noise handshake does not end with the pinned key of the portal, or when the server closes
 * the session with 4401. The connection then stops: nothing is sent to that address until the user starts a re-enrollment from
 * the screen of the portal. The reason is stored in the column `reenroll_reason`, so the state survives a restart and the app
 * does not knock again at the door of a server that has another key. A revoked device (4403) and an invalid address stay in
 * [LinkState.REENROLL].
 */
object KeyChange {
    /** The text that the native library gives when message 2 of the handshake does not authenticate with the pinned key. */
    private const val HANDSHAKE_FAILURE = "authentication failed"

    /** The stored reasons of this state start with this text. */
    const val PREFIX = "key_changed"

    /** The reason to store. [detail] is a short human text (no key, no frame). */
    fun reason(detail: String): String = "$PREFIX: $detail"

    /** True when the message of the native exception says that the handshake failed on the key. A malformed message is not a key change. */
    fun isHandshakeKeyFailure(message: String?): Boolean = message != null && message.contains(HANDSHAKE_FAILURE)

    fun isKeyChanged(reenrollReason: String?): Boolean = reenrollReason != null && reenrollReason.startsWith(PREFIX)

    /** The state that a stored reason stands for. A null reason has none. */
    fun stateOf(reenrollReason: String?): LinkState? = when {
        reenrollReason == null -> null
        isKeyChanged(reenrollReason) -> LinkState.KEY_CHANGED
        else -> LinkState.REENROLL
    }

    /** The text of the state without the technical prefix, for the detail line of a screen. */
    fun detail(reenrollReason: String): String = if (isKeyChanged(reenrollReason)) reenrollReason.removePrefix(PREFIX).removePrefix(":").trim() else reenrollReason
}
