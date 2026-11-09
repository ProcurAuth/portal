package app.portalinbox.portal

import app.portalinbox.data.Server

/**
 * How the phone shows one portal. This is the only thing that a screen, a notification or the approval prompt reads to
 * draw a portal: the nickname, the color, the monogram and the fingerprint. A screen never reads `Server.reportedName`
 * for that: the portal chooses that text, and the portal is not trusted with its own identity (proposal 2, rule 1).
 *
 * All five values are local. The user chooses the nickname and the color when adding the portal. The fingerprint comes
 * from the pinned key, not from the wire.
 */
data class PortalIdentity(
    /** The id of the row of the portal (`Server.id`). */
    val id: Long,
    /** The name that the user gave. 1 to 16 characters, without hidden or bidi characters. */
    val nickname: String,
    /** Index 0 to 9 in [PortalPalette]. */
    val colorIndex: Int,
    /** The first two letters of the nickname, in upper case: the second cue for a person who does not see color. */
    val monogram: String,
    /** The fingerprint of the pinned key in the display form: five groups of 4 hexadecimal characters. */
    val fingerprint: String,
) {
    /** The first [groups] groups of the fingerprint (the banner of an approval shows 2). */
    fun shortFingerprint(groups: Int = BANNER_GROUPS): String = Fingerprint.short(fingerprint, groups)

    /** The color for the theme: see [PortalPalette.color]. */
    fun color(night: Boolean): Int = PortalPalette.color(colorIndex, night)

    /** The color of a text or a shape on top of [color]. */
    fun onColor(night: Boolean): Int = PortalPalette.onColor(night)

    /** The line of a notification or of a button that names the portal: `on Production`. */
    val onText: String get() = "on $nickname"

    companion object {
        /** The banner of the approval screen shows this number of groups. The footer shows all of them. */
        const val BANNER_GROUPS = 2

        /** A portal is "New" for this long after it was added (proposal 3.1, question Q10). */
        const val NEW_WINDOW_MS = 24L * 60 * 60 * 1000

        fun of(server: Server): PortalIdentity = PortalIdentity(
            id = server.id,
            nickname = server.nickname,
            colorIndex = server.colorIndex,
            monogram = PortalNaming.monogram(server.nickname),
            fingerprint = Fingerprint.display(server.keyFp),
        )

        /** True during the first 24 hours after the portal was added. A clock that went back makes it true too. */
        fun isNew(addedAtMs: Long, nowMs: Long): Boolean = nowMs - addedAtMs < NEW_WINDOW_MS
    }
}

/** A portal that the phone already holds, with the address that it was enrolled with: what [LookAlike] and [PortalAdmission] compare to. */
data class KnownPortal(val identity: PortalIdentity, val address: String) {
    /** The compact fingerprint of the pinned key. */
    val keyFp: String get() = Fingerprint.compact(identity.fingerprint)

    companion object {
        fun of(server: Server): KnownPortal = KnownPortal(PortalIdentity.of(server), server.address)
    }
}
