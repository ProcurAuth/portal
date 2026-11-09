package app.portalinbox.alert

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEvent
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.proto.RequestKind
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

    const val BACK_TITLE = "The agent is back"

    /**
     * "The agent is back" and "ops-agent answers again: K7QF". An approval shows its code, a question its title: never the
     * title of the view, which comes from the command.
     */
    fun agentBack(item: Item): Content {
        val from = VisibleText.escapeLine(item.request.from.label)
        val what = if (item.kind == RequestKind.APPROVAL) item.request.approval?.code?.let { VisibleText.escapeLine(it) } ?: "request"
        else item.request.question?.title?.let { VisibleText.escapeLine(it) } ?: "question"
        return Content(
            title = BACK_TITLE,
            text = "$from answers again: $what",
            publicTitle = BACK_TITLE,
            publicText = if (item.kind == RequestKind.APPROVAL) APPROVAL_PUBLIC_TEXT else QUESTION_PUBLIC_TEXT,
        )
    }

    const val REPLY_PUBLIC_TITLE = "Reply from an agent"

    /**
     * "Reply from ops-agent" and the code or the title of the item. The text of the turn never shows: it is the text of a
     * third party, and a notification can show on a locked screen.
     */
    fun reply(item: Item): Content {
        val from = VisibleText.escapeLine(item.request.from.label)
        val what = if (item.kind == RequestKind.APPROVAL) "Approval " + (item.request.approval?.code?.let { VisibleText.escapeLine(it) } ?: "")
        else item.request.question?.title?.let { VisibleText.escapeLine(it) } ?: "Question"
        return Content(
            title = "Reply from $from",
            text = what.trim(),
            publicTitle = REPLY_PUBLIC_TITLE,
            publicText = QUESTION_PUBLIC_TEXT,
        )
    }
}

/** What to post, and what to withdraw, for an event of an item. Pure Kotlin. */
object NotificationPlan {
    const val CH_REQUESTS = "approvals"
    const val CH_REQUESTS_QUIET = "approvals_quiet"
    const val CH_DIGEST = "digest"
    const val CH_REPLIES = "replies"

    /** A reply or "The agent is back" in quiet hours or at a low urgency: the same texts, no sound. */
    const val CH_REPLIES_QUIET = "replies_quiet"

    /** The ids of the notifications of the items. The tag tells which item. */
    const val ID_REQUEST = 3000
    const val ID_DIGEST = 3001
    const val ID_REPLY = 3002

    /** The screen that a tap on the notification opens. A question has no screen before lot A4: it opens the home list. */
    enum class Target { HOME, APPROVAL, THREAD }

    /** What the plan needs besides the event: the quiet hours of the account and the time. [nowMs] is the time of the phone. */
    data class Env(val quiet: QuietPrefs?, val nowMs: Long) {
        companion object {
            /** No quiet hours: only the flag of the frame and the urgency decide. */
            val NONE = Env(null, 0L)
        }
    }

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
        val notificationId: Int = ID_REQUEST,
        val target: Target = Target.HOME,
        /** The notification has the action Open. A question has it, in place of Decline. */
        val openAction: Boolean = false,
    )

    fun tag(item: Item): String = item.request.chainId

    private fun targetOf(item: Item) = if (item.kind == RequestKind.APPROVAL) Target.APPROVAL else Target.HOME

    private fun plan(item: Item, refresh: Boolean, env: Env): Plan {
        val r = item.request
        val quiet = QuietHours.silent(r.quiet, r.urgency, env.quiet, env.nowMs)
        return Plan(
            tag = tag(item),
            requestId = item.requestId,
            serverId = item.serverId,
            content = NotificationText.of(item),
            channel = if (quiet) CH_REQUESTS_QUIET else CH_REQUESTS,
            ring = !quiet && !refresh,
            declineAction = item.kind == RequestKind.APPROVAL,
            refresh = refresh,
            target = targetOf(item),
            openAction = item.kind != RequestKind.APPROVAL,
        )
    }

    /** "The agent is back": one notification with the tag of the chain. It has no sound in quiet hours. No Decline action. */
    private fun back(item: Item, env: Env): Plan {
        val r = item.request
        val quiet = QuietHours.silent(r.quiet, r.urgency, env.quiet, env.nowMs)
        return Plan(
            tag = tag(item),
            requestId = item.requestId,
            serverId = item.serverId,
            content = NotificationText.agentBack(item),
            channel = if (quiet) CH_REPLIES_QUIET else CH_REPLIES,
            ring = false,
            declineAction = false,
            refresh = false,
            target = targetOf(item),
        )
    }

    /**
     * The notification for an event, or null when there is none. A new request notifies. A request that comes back (`Revived`)
     * says "The agent is back" when the phone already notified, and notifies like a new one when it did not. A request that
     * changes (`Updated`) refreshes the notification without sound when the phone already notified.
     */
    fun forEvent(event: ItemEvent, env: Env = Env.NONE): Plan? = when (event) {
        is ItemEvent.NewRequest -> if (event.item.isOpen) plan(event.item, refresh = false, env) else null
        is ItemEvent.Revived ->
            if (!event.item.isOpen) null else if (event.item.notified) back(event.item, env) else plan(event.item, refresh = false, env)
        is ItemEvent.Updated -> if (event.item.isOpen) plan(event.item, refresh = event.item.notified, env) else null
        else -> null
    }

    /**
     * The notification for a turn of the agent, or null: a turn of the owner, an item that is gone or closed. The text of the
     * turn is not in the notification. One notification for each item: a new turn replaces the last one.
     */
    fun forMessage(event: ItemEvent, item: Item?, env: Env = Env.NONE): Plan? {
        if (event !is ItemEvent.AgentMessage || item == null || !item.isOpen) return null
        val quiet = QuietHours.silent(false, item.request.urgency, env.quiet, env.nowMs)
        return Plan(
            tag = item.requestId,
            requestId = item.requestId,
            serverId = item.serverId,
            content = NotificationText.reply(item),
            channel = if (quiet) CH_REPLIES_QUIET else CH_REPLIES,
            ring = false,
            declineAction = false,
            refresh = false,
            notificationId = ID_REPLY,
            target = Target.THREAD,
        )
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
