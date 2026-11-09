package app.portalinbox.testsupport

import app.portalinbox.data.Server
import app.portalinbox.portal.Fingerprint
import app.portalinbox.portal.FingerprintSource
import app.portalinbox.portal.KnownPortal
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PortalNaming
import java.security.MessageDigest

/**
 * A JVM version of the fingerprint, for the tests only. The real one runs in Rust through JNI (`inbox-core`
 * `key_fingerprint`). `FingerprintTest` proves that this fake gives the golden values of the Rust tests.
 */
object ReferenceFingerprint : FingerprintSource {
    override fun of(publicKey: ByteArray): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(publicKey)
        return digest.take(10).joinToString("") { "%02x".format(it) }.chunked(4).joinToString(" ")
    }
}

object Portals {
    /** A 32-byte key whose last two bytes are [n]. */
    fun key(n: Int): ByteArray = ByteArray(32).also {
        it[30] = (n shr 8).toByte()
        it[31] = n.toByte()
    }

    fun fp(n: Int): String = Fingerprint.of(key(n), ReferenceFingerprint)

    fun identity(id: Long, nickname: String, color: Int, keyNo: Int = id.toInt()): PortalIdentity =
        PortalIdentity(id, nickname, color, PortalNaming.monogram(nickname), fp(keyNo))

    fun known(id: Long, nickname: String, color: Int, address: String, keyNo: Int = id.toInt()): KnownPortal =
        KnownPortal(identity(id, nickname, color, keyNo), address)

    fun server(id: Long = 1, nickname: String = "Production", color: Int = 0, keyNo: Int = 1, address: String = "https://portal.example.org"): Server = Server(
        id = id, address = address, reportedName = "Whatever the portal says", deviceId = "d$id", deviceName = "Pixel",
        deviceKeySealed = ByteArray(1), serverPublicKey = key(keyNo), reenrollReason = null, heartbeatMs = 20_000, offlineWarningMs = 180_000,
        userName = "alice", nickname = nickname, colorIndex = color, sortIndex = id.toInt(),
        keyFp = Fingerprint.compact(fp(keyNo)), mutedUntilMs = null, hideNameOnLock = false, retentionDays = null, lastSyncMs = null,
        prevKeyFp = null, keyChangedMs = null, pendingPrefs = null, addedAtMs = 1_000,
    )
}
