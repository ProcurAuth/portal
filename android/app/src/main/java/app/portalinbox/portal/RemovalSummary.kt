package app.portalinbox.portal

import app.portalinbox.data.ItemRepository

/**
 * What "Remove" erases for one portal: the text before the one confirmation. Pure Kotlin. The counts come from the
 * repository before the delete. The delete itself is the cascade of the foreign keys of the table `servers`.
 */
data class RemovalSummary(
    /** Requests that wait for the user (approvals and questions). They are lost. */
    val openItems: Int,
    /** Answers that the phone did not send yet. They are lost. */
    val unsentAnswers: Int,
    /** All items of the portal, open and finished (the history). */
    val historyItems: Int,
    /** Alerts of the portal on this phone. */
    val alerts: Int,
) {
    /** True when the portal holds nothing of value: the screen can say so. */
    val empty: Boolean get() = openItems == 0 && unsentAnswers == 0 && historyItems == 0 && alerts == 0

    companion object {
        fun of(repo: ItemRepository, serverId: Long, alertCount: Int): RemovalSummary {
            val items = repo.items(serverId)
            return RemovalSummary(
                openItems = items.count { it.isOpen },
                unsentAnswers = repo.pendingOutbox(serverId).size,
                historyItems = items.count { !it.isOpen },
                alerts = alertCount,
            )
        }
    }
}
