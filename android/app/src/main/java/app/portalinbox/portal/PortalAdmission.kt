package app.portalinbox.portal

/**
 * The rules that decide if the phone takes one more portal. Pure Kotlin.
 * `Store.addServer` runs [check] inside its transaction. The `UNIQUE` columns of the table `servers` are the second
 * guard: if both disagree, the database wins and the add fails.
 *
 * - A portal whose pinned key is already held is refused, whatever its address (two addresses of one portal are two
 *   enrollments of one key). "Change address" is the way to move a portal.
 * - The phone holds at most [MAX_PORTALS] portals. The 11th add is refused.
 * - The nickname must be valid and unique ([PortalNaming]). The color must be free. If the caller gives no color, the
 *   first free color is used ([PortalPalette.nextFree]).
 */
object PortalAdmission {
    /** The size of the palette: each portal has its own color. */
    const val MAX_PORTALS = PortalPalette.SIZE

    sealed interface Decision {
        /** [nicknameKey] is the normalized nickname for the unique column. */
        data class Admit(val nickname: String, val nicknameKey: String, val colorIndex: Int) : Decision

        data class Refuse(val refusal: Refusal) : Decision
    }

    sealed interface Refusal {
        /** The key is the key of [portal]. The screen offers to open that portal. */
        data class DuplicateKey(val portal: KnownPortal) : Refusal

        /** The phone holds [MAX_PORTALS] portals. */
        object CapReached : Refusal

        data class BadName(val invalid: PortalNaming.Check.Invalid) : Refusal

        /** The color index is not 0 to 9. */
        data class BadColor(val colorIndex: Int) : Refusal

        /** Another portal has this color. */
        data class ColorTaken(val colorIndex: Int, val portal: KnownPortal) : Refusal
    }

    /**
     * Decides. [existing] are the portals of the phone in their order of addition (the last one is the most recent). [keyFp]
     * is the fingerprint of the key to pin, in any form. [colorIndex] null means "the first free color".
     */
    fun check(existing: List<KnownPortal>, keyFp: String, nickname: String, colorIndex: Int?): Decision {
        val fp = Fingerprint.parse(keyFp) ?: throw IllegalArgumentException("not a fingerprint")
        existing.firstOrNull { it.keyFp == fp }?.let { return Decision.Refuse(Refusal.DuplicateKey(it)) }
        if (existing.size >= MAX_PORTALS) return Decision.Refuse(Refusal.CapReached)
        val named = when (val c = PortalNaming.validate(nickname, existing.map { it.identity })) {
            is PortalNaming.Check.Invalid -> return Decision.Refuse(Refusal.BadName(c))
            is PortalNaming.Check.Valid -> c
        }
        val color = if (colorIndex == null) {
            PortalPalette.nextFree(existing.map { it.identity.colorIndex }, existing.lastOrNull()?.identity?.colorIndex)
                ?: return Decision.Refuse(Refusal.CapReached)
        } else {
            if (!PortalPalette.isValid(colorIndex)) return Decision.Refuse(Refusal.BadColor(colorIndex))
            existing.firstOrNull { it.identity.colorIndex == colorIndex }?.let { return Decision.Refuse(Refusal.ColorTaken(colorIndex, it)) }
            colorIndex
        }
        return Decision.Admit(named.nickname, named.key, color)
    }
}
