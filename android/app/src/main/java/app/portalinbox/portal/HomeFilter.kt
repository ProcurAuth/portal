package app.portalinbox.portal

import app.portalinbox.data.OpenCount

/**
 * The filter of the home list: all portals, or one portal. Pure Kotlin.
 *
 * The filter is never stored. A new start of the app shows All: a stored filter could hide the request of
 * another portal for days.
 */
sealed interface HomeFilter {
    object All : HomeFilter {
        override fun toString() = "All"
    }

    data class Portal(val id: Long) : HomeFilter

    /** True when a row of this server shows with the filter. */
    fun matches(serverId: Long): Boolean = when (this) {
        All -> true
        is Portal -> id == serverId
    }

    /** The id of the portal, or null for All. The argument of the storage queries. */
    val portalId: Long? get() = (this as? Portal)?.id

    /** The filter to use when the portals are [existing]: a filter on a portal that was removed falls back to All. */
    fun resolve(existing: Collection<Long>): HomeFilter = if (this is Portal && id !in existing) All else this

    companion object {
        /** The filter at each start of the app. */
        val AT_START: HomeFilter = All

        /** The filter after a tap on [tapped]. A second tap on the chip of the chosen portal goes back to All. */
        fun tap(current: HomeFilter, tapped: HomeFilter): HomeFilter = if (current == tapped && tapped is Portal) All else tapped

        /** "To answer · Production" with a filter on one portal, "To answer" without. */
        fun sectionTitle(base: String, nickname: String?): String = if (nickname == null) base else "$base · $nickname"

        /** The empty state of a filter on one portal. */
        fun nothingFrom(nickname: String): String = "Nothing from $nickname"
    }
}

/**
 * The numbers on the filter chips: unread alerts plus open items, for each portal and for All. Only the
 * portals that exist count: a row of a removed portal is not shown, so it is not counted.
 */
data class HomeCounts(val perPortal: Map<Long, Int>, val all: Int) {
    /** The number for the chip of [filter]. */
    fun of(filter: HomeFilter): Int = when (filter) {
        HomeFilter.All -> all
        is HomeFilter.Portal -> perPortal[filter.id] ?: 0
    }

    companion object {
        fun build(portalIds: Collection<Long>, open: Map<Long, OpenCount>, unreadAlerts: Map<Long, Int>): HomeCounts {
            val per = portalIds.associateWith { (open[it]?.total ?: 0) + (unreadAlerts[it] ?: 0) }
            return HomeCounts(per, per.values.sum())
        }
    }
}
