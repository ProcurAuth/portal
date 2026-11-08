package app.portalinbox.alert

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEvent
import app.portalinbox.proto.RequestKind
import app.portalinbox.proto.Urgency
import app.portalinbox.ui.VisibleText

/**
 * The texts of the notifications of the items (CONF 5.3). Pure Kotlin: no Context.
 *
 * The phone builds these texts. They name the code, the key and the service, and nothing else. They hold no command, no
 * title from the view, no field of the operation and no explanation: a notification can show on a locked screen, and a
 * text of an agent must not reach it. The public version (shown on a locked screen) names the code only.
 */
object NotificationText {
    data class Content(val title: String, val text: String, val publicTitle: String, val publicText: String)

    const val APPROVAL_PUBLIC_TEXT = "Content hidden. Unlock to review."
    const val QUESTION_PUBLIC_TEXT = "Content hidden. Unlock to read."
    const val QUESTION_TITLE = "Question from an agent"
    const val DECLINE_LABEL = "Decline"
    const val OPEN_LABEL = "Open"

    /** "Approval requested · K7QF" and "key ops-agent · ClickHouse Dash". */
    fun approval(item: Item): Content {
        val a = item.request.approval
        val code = a?.code?.let { VisibleText.escapeLine(it) } ?: "request"
        val key = VisibleText.escapeLine(item.request.from.keyName)
        val service = a?.service?.let { VisibleText.escapeLine(it) }
        return Content(
            title = "Approval requested · $code",
            text = listOfNotNull("key $key", service).joinToString(" · "),
            publicTitle = "Approval $code",
            publicText = APPROVAL_PUBLIC_TEXT,
        )
    }

    /** "Question from an agent" and "<title> · ops-agent". The title is the title of the question, never its body. */
    fun question(item: Item): Content {
        val title = item.request.question?.title?.let { VisibleText.escapeLine(it) }
        val from = VisibleText.escapeLine(item.request.from.label)
        return Content(
            title = QUESTION_TITLE,
            text = listOfNotNull(title, from).joinToString(" · "),
            publicTitle = QUESTION_TITLE,
            publicText = QUESTION_PUBLIC_TEXT,
        )
    }

    fun of(item: Item): Content = if (item.kind == RequestKind.APPROVAL) approval(item) else question(item)
}

/** What to post, and what to withdraw, for an event of an item. Pure Kotlin. */
object NotificationPlan {
    const val CH_REQUESTS = "approvals"
    const val CH_REQUESTS_QUIET = "approvals_quiet"
    const val CH_DIGEST = "digest"
    const val CH_REPLIES = "replies"

    data class Plan(
        /** The notification tag: the chain id. A request that replaces another one in its chain replaces the notification. */
        val tag: String,
        val requestId: String,
        val serverId: Long,
        val content: NotificationText.Content,
        val channel: String,
        /** The phone rings once (the level Ring). */
        val ring: Boolean,
        /** The notification has the action Decline. Only an approval has it, and never Approve. */
        val declineAction: Boolean,
        /** A refresh of a notification that is already there: no sound, no new heads-up. */
        val refresh: Boolean,
    )

    fun tag(item: Item): String = item.request.chainId

    private fun plan(item: Item, refresh: Boolean): Plan {
        val r = item.request
        val quiet = r.quiet || r.urgency == Urgency.LOW
        return Plan(
            tag = tag(item),
            requestId = item.requestId,
            serverId = item.serverId,
            content = NotificationText.of(item),
            channel = if (quiet) CH_REQUESTS_QUIET else CH_REQUESTS,
            ring = !quiet && !refresh,
            declineAction = item.kind == RequestKind.APPROVAL,
            refresh = refresh,
        )
    }

    /**
     * The notification for an event, or null when there is none. A new request notifies. A request that comes
     * back (`Revived`) or changes (`Updated`) refreshes the notification without sound when the phone already notified, and
     * notifies like a new one when it did not.
     */
    fun forEvent(event: ItemEvent): Plan? = when (event) {
        is ItemEvent.NewRequest -> if (event.item.isOpen) plan(event.item, refresh = false) else null
        is ItemEvent.Revived -> if (event.item.isOpen) plan(event.item, refresh = event.item.notified) else null
        is ItemEvent.Updated -> if (event.item.isOpen) plan(event.item, refresh = event.item.notified) else null
        else -> null
    }

    /**
     * The tag to withdraw when [settled] became final, or null. When another open item of the same chain exists, its
     * notification took the tag over: withdrawing the tag would remove the notification of the newer request.
     */
    fun withdrawTag(settled: Item, openItems: List<Item>): String? {
        val tag = tag(settled)
        val other = openItems.any { it.isOpen && it.chainKey() == settled.chainKey() && it.requestId != settled.requestId }
        return if (other) null else tag
    }

    private fun Item.chainKey() = serverId to request.chainId
}
