package app.portalinbox.portal

/**
 * What the filter strip and the dots of the top bar show (proposal 3.2). Pure Kotlin: the screen only draws it.
 */
object FilterStrip {
    /** One chip. [portal] is null for the chip All. */
    data class Chip(
        val filter: HomeFilter,
        val portal: HomePortal?,
        val count: Int,
        val selected: Boolean,
    ) {
        /** A portal that is down shows a dashed chip with a dot. */
        val dashed: Boolean get() = portal?.health?.isDown == true

        /** The words of a screen reader: the name, the state, the number. */
        fun spoken(): String {
            val name = portal?.identity?.nickname ?: "All portals"
            val state = when (portal?.health) {
                PortalHealth.OFFLINE -> ", offline"
                PortalHealth.KEY_CHANGED -> ", key check failed"
                PortalHealth.RE_ENROLL -> ", must enroll again"
                else -> ""
            }
            val isNew = if (portal?.isNew == true) ", new" else ""
            val n = if (count == 1) "1 waiting" else "$count waiting"
            return "$name$isNew$state, $n" + if (selected) ", selected" else ""
        }
    }

    /**
     * All, then one chip per portal in the order of the user. The strip is useful with two portals or more. With one portal
     * it is empty: All and the portal would show the same rows.
     */
    fun chips(portals: List<HomePortal>, counts: HomeCounts, filter: HomeFilter): List<Chip> {
        if (portals.size < 2) return emptyList()
        return listOf(Chip(HomeFilter.All, null, counts.all, filter == HomeFilter.All)) +
            portals.map { Chip(HomeFilter.Portal(it.id), it, counts.of(HomeFilter.Portal(it.id)), filter == HomeFilter.Portal(it.id)) }
    }

    /** One dot of the top bar. Hollow when the portal is down. */
    data class Dot(val identity: PortalIdentity, val hollow: Boolean)

    /** One dot per portal, in the order of the user. It replaces the single connection dot. */
    fun dots(portals: List<HomePortal>): List<Dot> = portals.map { Dot(it.identity, it.health.isDown) }

    /** The words of a screen reader for the dots: the color alone says nothing. */
    fun dotsSpoken(portals: List<HomePortal>): String = when {
        portals.isEmpty() -> "No portal"
        portals.all { !it.health.isDown } -> "All portals connected"
        else -> portals.joinToString(". ") { p ->
            p.identity.nickname + when (p.health) {
                PortalHealth.OK -> " connected"
                PortalHealth.OFFLINE -> " offline"
                PortalHealth.KEY_CHANGED -> " key check failed"
                PortalHealth.RE_ENROLL -> " must enroll again"
            }
        }
    }
}
