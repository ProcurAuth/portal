package app.portalinbox.portal

/** How a screen lays out the 5 groups of a fingerprint: a few groups on each line, so a narrow screen never cuts a group. */
object FingerprintText {
    /** The groups of [fingerprint] (any form), [perLine] groups on a line, lines joined with a line feed. Returns the text unchanged if it is not a fingerprint. */
    fun lines(fingerprint: String, perLine: Int = 3): String {
        require(perLine in 1..Fingerprint.GROUPS) { "perLine must be 1 to ${Fingerprint.GROUPS}" }
        val compact = Fingerprint.parse(fingerprint) ?: return fingerprint
        return compact.chunked(Fingerprint.GROUP_LENGTH).chunked(perLine).joinToString("\n") { it.joinToString(" ") }
    }

    /** The same text for a screen reader: each group is spelled with spaces between the characters. */
    fun spoken(fingerprint: String): String {
        val compact = Fingerprint.parse(fingerprint) ?: return fingerprint
        return compact.chunked(Fingerprint.GROUP_LENGTH).joinToString(", ") { g -> g.toList().joinToString(" ") }
    }
}
