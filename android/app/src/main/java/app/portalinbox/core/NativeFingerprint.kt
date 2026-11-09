package app.portalinbox.core

import app.portalinbox.portal.FingerprintSource

/** The [FingerprintSource] that calls `inbox-core` through [Native]. JNI does not load in JVM tests. */
object NativeFingerprint : FingerprintSource {
    override fun of(publicKey: ByteArray): String = Native.fingerprint(publicKey)
}
