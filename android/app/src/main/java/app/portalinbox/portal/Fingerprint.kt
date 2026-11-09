package app.portalinbox.portal

import app.portalinbox.core.NativeFingerprint

/** Where the text of a fingerprint comes from. The real source is Rust ([NativeFingerprint]); a JVM test injects a fake. */
fun interface FingerprintSource {
    /** The fingerprint of a public key in the display form: five groups of 4 hexadecimal characters. */
    fun of(publicKey: ByteArray): String
}

/**
 * The fingerprint of the pinned key of a portal (proposal 4.2). It is the first 80 bits of the SHA-256 digest of the
 * Noise public key of the server. The hash runs in `inbox-core` through JNI, so the rule exists once and is the same
 * as the one on the page that makes the enrollment code. This object does not hash: it checks the key, checks the
 * text that comes back, and gives the forms that the screens and the database use.
 *
 * Two forms exist. The display form has five groups with one space between them: `6668 7aad f862 bd77 6c8f`.
 * The compact form has 20 characters and no space: it is the value of the column `key_fp`.
 */
object Fingerprint {
    /** The length of a Noise public key. */
    const val KEY_BYTES = 32
    const val GROUPS = 5
    const val GROUP_LENGTH = 4
    const val COMPACT_LENGTH = GROUPS * GROUP_LENGTH

    /** The display form of the fingerprint of [publicKey]. Throws [IllegalArgumentException] for a key that is not 32 bytes. */
    fun of(publicKey: ByteArray, source: FingerprintSource = NativeFingerprint): String {
        require(publicKey.size == KEY_BYTES) { "a public key has $KEY_BYTES bytes, not ${publicKey.size}" }
        val text = source.of(publicKey)
        check(isDisplay(text)) { "the fingerprint source gave a text that is not five groups of 4 hexadecimal characters" }
        return text
    }

    /** The compact form of the fingerprint of [publicKey]: what the database stores and compares. */
    fun compactOf(publicKey: ByteArray, source: FingerprintSource = NativeFingerprint): String = compact(of(publicKey, source))

    /** The compact form of a display or compact fingerprint: lower case, no space. */
    fun compact(fingerprint: String): String = fingerprint.filterNot { it == ' ' }.lowercase()

    /** The display form of a compact fingerprint. Throws [IllegalArgumentException] for a text that is not 20 hexadecimal characters. */
    fun display(compact: String): String {
        val c = compact.lowercase()
        require(isCompact(c)) { "a fingerprint has $COMPACT_LENGTH hexadecimal characters" }
        return c.chunked(GROUP_LENGTH).joinToString(" ")
    }

    /**
     * Reads a fingerprint that a person typed or that a screen holds: with or without spaces, upper or lower case.
     * Returns the compact form, or null when the text is not 20 hexadecimal characters.
     */
    fun parse(text: String): String? = compact(text.trim()).takeIf { isCompact(it) }

    /** The first [groups] groups of a fingerprint, in the display form: the banner of an approval shows 2 of 5. */
    fun short(fingerprint: String, groups: Int): String {
        require(groups in 1..GROUPS) { "groups must be 1 to $GROUPS" }
        val c = parse(fingerprint) ?: throw IllegalArgumentException("not a fingerprint")
        return c.chunked(GROUP_LENGTH).take(groups).joinToString(" ")
    }

    /** True when both texts are the same fingerprint, whatever their form. False when one of them is not a fingerprint. */
    fun same(a: String, b: String): Boolean {
        val x = parse(a) ?: return false
        return x == parse(b)
    }

    fun isCompact(text: String): Boolean = text.length == COMPACT_LENGTH && text.all { it in '0'..'9' || it in 'a'..'f' }

    fun isDisplay(text: String): Boolean {
        val groups = text.split(' ')
        return groups.size == GROUPS && groups.all { g -> g.length == GROUP_LENGTH && g.all { it in '0'..'9' || it in 'a'..'f' } }
    }
}
