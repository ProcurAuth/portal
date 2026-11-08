package app.portalinbox.ui

import app.portalinbox.data.Item
import app.portalinbox.proto.Channel
import app.portalinbox.proto.Decision
import app.portalinbox.proto.FormQuestionType
import app.portalinbox.proto.InputType
import app.portalinbox.proto.Intent
import app.portalinbox.proto.Interaction
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.LeaseMode
import app.portalinbox.proto.RequestKind
import java.time.ZoneId

/** The words about an item that the list and the screens share. Pure Kotlin, English, no Context. */
object ItemTexts {
    /** "Asked 6 d ago". */
    fun asked(item: Item, nowMs: Long): String = "Asked " + Ages.ago(item.request.createdAtMs, nowMs)

    /** "agent active 3 min ago". */
    fun agentActive(item: Item, nowMs: Long): String = "agent active " + Ages.ago(item.request.agentSeenAtMs, nowMs)

    /** "Asked 3 min ago · agent active 3 min ago". */
    fun headerLine(item: Item, nowMs: Long): String = asked(item, nowMs) + " · " + agentActive(item, nowMs)

    /** "Lease until 11-14 08:00", or "Open until 11-14 08:00" for a request with an end time. Null when the frame has none. */
    fun leaseLine(item: Item, zone: ZoneId): String? {
        val r = item.request
        return when (r.leaseMode) {
            LeaseMode.LEASE -> r.leaseUntilMs?.let { "Lease until " + Ages.dayTime(it, zone) }
            LeaseMode.UNTIL -> r.untilAtMs?.let { "Open until " + Ages.dayTime(it, zone) }
        }
    }

    /** The sender line: "ops-agent · key of alice · ClickHouse Dash". Hidden characters show as marks. */
    fun senderLine(item: Item): String {
        val f = item.request.from
        val service = item.request.approval?.service
        return listOfNotNull(f.label, "key of " + f.owner, service).joinToString(" · ") { VisibleText.escapeLine(it) }
    }

    /** The title of an item: the title of the view, or of the question. "Invalid request" when the phone does not trust it. */
    fun title(item: Item): String = when {
        item.kind == RequestKind.QUESTION -> VisibleText.escapeLine(item.request.question?.title ?: "Question")
        item.validity.isValid -> VisibleText.escapeLine(item.view?.title ?: "Approval")
        else -> "Invalid request"
    }

    // ------------------------------------------------------------ context lines of the approval

    /** The lines under the explanation: the earlier approval of the same effect, the follow-up, the suggestion. */
    fun contextLines(item: Item, nowMs: Long): List<String> {
        val a = item.request.approval ?: return emptyList()
        return buildList {
            a.previous?.let { add("Same effect approved " + Ages.ago(it.approvedAtMs, nowMs) + if (it.consumed) " (it ran)." else ".") }
            a.follows?.let {
                add("Follows " + it.code + ": " + if (it.intent == Intent.CHANGE_REQUEST) "a change was asked." else "a comment was left.")
            }
            a.hint?.let { h ->
                val pct = h.confidence?.let { c -> ", " + Math.round(c * 100) + " %" } ?: ""
                add("Suggestion from " + VisibleText.escapeLine(h.source) + " (not checked): " + VisibleText.escapeLine(h.suggested) + pct)
            }
        }
    }

    // ------------------------------------------------------------ closed items

    /** The words of the state of a closed approval: what ended it. Null while the item is open. */
    fun closedNotice(item: Item): String? {
        val s = item.settled
        return when (item.state) {
            ItemState.PENDING, ItemState.REPLIED -> null
            ItemState.ANSWERED -> when (s?.decision) {
                Decision.APPROVE -> "Approved" + by(item)
                Decision.DENY -> "Declined" + by(item)
                null -> "Answered" + by(item)
            }
            ItemState.ABANDONED -> "The agent no longer answers. You cannot answer this request."
            ItemState.EXPIRED -> "This request expired. No decision was made."
            ItemState.CANCELLED -> "The agent cancelled this request."
            ItemState.SUPERSEDED -> "A newer request replaced this one."
        }
    }

    private fun by(item: Item): String {
        val b = item.settled?.by ?: return "."
        return when (b.via) {
            Channel.PHONE -> " on " + (b.deviceName?.let { VisibleText.escapeLine(it) } ?: "a phone") + "."
            Channel.WEB -> " on the web."
        }
    }

    /** The word of the chip of a closed item: `approved`, `declined`, `abandoned`... */
    fun stateWord(item: Item): String = when (item.state) {
        ItemState.PENDING -> "waiting"
        ItemState.REPLIED -> "waiting"
        ItemState.ANSWERED -> when (item.settled?.decision) {
            Decision.APPROVE -> "approved"
            Decision.DENY -> "declined"
            null -> "answered"
        }
        ItemState.ABANDONED -> "abandoned"
        ItemState.EXPIRED -> "expired"
        ItemState.CANCELLED -> "cancelled"
        ItemState.SUPERSEDED -> "replaced"
    }

    /** The second line of a closed row: the reason in words. A reason that this version does not know shows as it is. */
    fun reasonLine(item: Item): String {
        val s = item.settled
        return when (item.state) {
            ItemState.ABANDONED -> "The agent no longer answers"
            ItemState.SUPERSEDED -> "Replaced by a newer request"
            ItemState.CANCELLED -> "Cancelled by the agent"
            ItemState.EXPIRED -> when (s?.reason) {
                "max_lifetime" -> "Life limit reached"
                "until_reached" -> "The end time passed"
                else -> "No decision"
            }
            ItemState.ANSWERED -> closedNotice(item) ?: "Answered"
            else -> s?.reason?.let { VisibleText.escapeLine(it) } ?: ""
        }
    }

    // ------------------------------------------------------------ question facts for the list

    /** "3 choices", "3 questions", "text answer", "confirm": what kind of answer a question wants. */
    fun inputLine(item: Item): String? {
        val input = item.request.question?.input ?: return null
        return when (input.type) {
            InputType.CHOICE -> "${input.options.size} choices"
            InputType.CONFIRM -> "confirm"
            InputType.TEXT -> "text answer"
            InputType.NONE -> "no answer needed"
            InputType.FORM -> input.questions?.let { qs ->
                val text = qs.count { it.type == FormQuestionType.TEXT }
                if (text == qs.size) "${qs.size} text questions" else "${qs.size} questions"
            }
        }
    }

    fun isAutomatic(item: Item): Boolean = item.request.from.interaction == Interaction.ONE_SHOT
}
