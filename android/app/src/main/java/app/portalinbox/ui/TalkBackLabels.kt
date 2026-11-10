package app.portalinbox.ui

import app.portalinbox.data.Item
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.RequestKind

/**
 * The words that a screen reader says for the item rows and the thread (DIAL 4.8). Pure Kotlin.
 *
 * Rules: full words, no short forms ("3 minutes ago", not "3 min"); the age and the state are text, never a color only; a
 * hidden character shows as its mark, so the reader says what the eye would see; the text of a third party is introduced
 * as "not checked".
 */
object TalkBackLabels {
    private fun plural(n: Int, one: String) = if (n == 1) "1 $one" else "$n ${one}s"

    /**
     * The label of a row of the home list: the portal, kind, sender, ages, state, number of choices. [portal] is the nickname
     * of the portal (lot MP3). [reason] says that the portal is down ("Offline 3 h. answer waits").
     */
    fun itemRow(item: Item, nowMs: Long, portal: String? = null, reason: String? = null, label: String? = null): String {
        val prefix = portal?.let { "Portal ${VisibleText.escapeLine(it)}. " } ?: ""
        return prefix + itemRowText(item, nowMs, label ?: item.request.from.label) + (reason?.let { ". $it" } ?: "")
    }

    private fun itemRowText(item: Item, nowMs: Long, label: String): String {
        val r = item.request
        val from = VisibleText.escapeLine(label)
        val asked = "asked " + Ages.spoken(r.createdAtMs, nowMs)
        val active = "agent active " + Ages.spoken(r.agentSeenAtMs, nowMs)
        val state = when {
            item.state == ItemState.REPLIED -> "waiting for the agent to answer"
            item.isOpen -> "waiting for you"
            else -> ItemTexts.stateWord(item)
        }
        return when {
            item.kind == RequestKind.APPROVAL -> {
                val a = r.approval
                val code = a?.code?.let { VisibleText.escapeLine(it) }
                val service = a?.service?.let { VisibleText.escapeLine(it) }
                val head = "Approval" + (code?.let { " $it" } ?: "") + " from $from"
                val invalid = if (!item.validity.isValid) ", invalid request" else ""
                listOfNotNull(head, service, asked, active, state).joinToString(", ") + invalid
            }
            ItemTexts.isAutomatic(item) -> {
                val title = VisibleText.escapeLine(r.question?.title ?: "Decision")
                listOfNotNull("Decision from the automatic system $from", title, asked, state, options(item)).joinToString(", ")
            }
            else -> {
                val title = VisibleText.escapeLine(r.question?.title ?: "Question")
                listOfNotNull("Question from the agent $from", title, asked, active, state, options(item)).joinToString(", ")
            }
        }
    }

    private fun options(item: Item): String? {
        val input = item.request.question?.input ?: return null
        val n = input.questions?.size
        return when {
            n != null -> plural(n, "question")
            input.options.isNotEmpty() -> plural(input.options.size, "option")
            else -> null
        }
    }

    /** The label of a bubble of the thread. The text of the agent is introduced as "not checked". */
    fun turn(entry: ThreadState.Entry, agent: String, text: String): String = when (entry) {
        is ThreadState.Entry.Agent -> "$agent, ${entry.whenText}, not checked. $text"
        is ThreadState.Entry.User -> {
            val state = entry.line?.let { ". $it" } ?: ""
            "You, ${entry.whenText}. $text$state"
        }
    }

    /** The label of the field of the thread. A blocked field says why. */
    fun composer(c: ThreadState.Composer): String =
        if (c.enabled) "Write to the agent" else "Message field. Blocked. " + (c.hint ?: c.placeholder)

    fun sendButton(c: ThreadState.Composer, hasText: Boolean): String = when {
        !c.enabled -> "Send. Not available now."
        !hasText -> "Send. Write a message first."
        else -> "Send the message to the agent"
    }

    /** The label of the line "ops-agent is writing". */
    fun writing(agent: String): String = "$agent is writing its answer"

    /** The label of the header of the thread: the title, the sender and the state in one sentence. */
    fun threadHeader(screen: ThreadState.Screen): String =
        listOfNotNull(screen.title, screen.senderLine, screen.chip?.word, screen.statusSpoken.ifEmpty { null }).joinToString(". ")

    // ------------------------------------------------------------ the screen of a question (lot A4)

    /**
     * The label of the header of a question: "Question from the agent triage, asked 6 days ago, agent active 3 minutes ago, 3
     * options". An automatic system has no agent that is active: "Decision from the automatic system jev, asked 12 minutes ago,
     * last computed 12 minutes ago, 3 options". The sender text is a text of a third party: its hidden characters show as marks.
     */
    fun questionHeader(item: Item, nowMs: Long, label: String = item.request.from.label): String {
        val r = item.request
        val from = VisibleText.escapeLine(label)
        val title = VisibleText.escapeLine(r.question?.title ?: "Question")
        val asked = "asked " + Ages.spoken(r.createdAtMs, nowMs)
        val automatic = ItemTexts.isAutomatic(item)
        val seen = if (automatic) "last computed " + Ages.spoken(r.agentSeenAtMs, nowMs) else "agent active " + Ages.spoken(r.agentSeenAtMs, nowMs)
        val head = if (automatic) "Decision from the automatic system $from" else "Question from the agent $from"
        return listOfNotNull(head, title, asked, seen, options(item)).joinToString(", ")
    }

    /** The label of one block of a form: "Question 2 of 3, Retention. Choose one. Not answered". */
    fun block(number: Int, total: Int, header: String?, rule: String, answered: Boolean): String =
        listOfNotNull(
            if (total > 1) "Question $number of $total" else null,
            header?.let { VisibleText.escapeLine(it) },
        ).joinToString(", ").let { if (it.isEmpty()) "" else "$it. " } + rule + " " + if (answered) "Answered" else "Not answered"

    /** The label of the Send button: the portal, and why it is off. */
    fun questionSend(s: QuestionState.Screen, portal: String?): String {
        val on = portal?.let { " on " + VisibleText.escapeLine(it) } ?: ""
        return if (s.sendEnabled) s.sendWord + on else s.sendWord + on + ". Not available now. " + (s.bottomHint ?: "")
    }

    /** The label of the row "Other answer": it says that the text excludes the options and that the agent reads it. */
    fun otherRow(exclusive: Boolean): String =
        QuestionState.OTHER_WORD + ". " + QuestionState.OTHER_NOTE + (if (exclusive) " It replaces the options." else "") + " " + QuestionState.SECRET_WARNING
}
