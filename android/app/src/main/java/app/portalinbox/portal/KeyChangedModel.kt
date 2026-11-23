package app.portalinbox.portal

import app.portalinbox.data.Server
import app.portalinbox.service.KeyChange

/**
 * The key-changed block of the portal screen. Pure Kotlin.
 *
 * The phone cannot know the new key: a failed handshake gives no key. So the block shows the old fingerprint, says that the
 * new one shows after the user enters a new code, and offers the two choices: enter a new code (re-enrollment in place) or
 * remove the portal.
 */
data class KeyChangedModel(
    /** The fingerprint that the phone pinned, in groups. */
    val oldFingerprint: String,
    /** The key before the last re-enrollment with another key, in groups. Null when there was none. */
    val previousFingerprint: String?,
    /** When the last re-enrollment with another key happened (phone clock). */
    val previousChangedMs: Long?,
    /** True: the stop comes from the key (handshake failure or close 4401). False: the device was revoked, or the address is not valid. */
    val keyProblem: Boolean,
    /** The text that the connection stored for the stop, without the technical prefix. */
    val detail: String?,
) {
    companion object {
        /** The block for [server], or null when the connection is not stopped. */
        fun of(server: Server): KeyChangedModel? {
            val reason = server.reenrollReason ?: return null
            return KeyChangedModel(
                oldFingerprint = Fingerprint.display(server.keyFp),
                previousFingerprint = server.prevKeyFp?.let { Fingerprint.display(it) },
                previousChangedMs = server.keyChangedMs,
                keyProblem = KeyChange.isKeyChanged(reason),
                detail = KeyChange.detail(reason).takeIf { it.isNotEmpty() },
            )
        }
    }
}
