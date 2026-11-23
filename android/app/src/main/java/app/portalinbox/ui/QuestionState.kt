package app.portalinbox.ui

import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.AnswerSheet
import app.portalinbox.data.BlockKind
import app.portalinbox.data.FlagsMatrix
import app.portalinbox.data.Item
import app.portalinbox.data.OutboxAnswer
import app.portalinbox.data.Sheet
import app.portalinbox.data.SheetDraft
import app.portalinbox.proto.Hint
import app.portalinbox.proto.InputType
import app.portalinbox.proto.Interaction
import app.portalinbox.proto.RequestKind

/**
 * What the screen of a question shows and lets the owner do. Pure Kotlin: the Activity gives
 * the facts and draws the answer.
 *
 * Rules:
 * - The rows under the options come from [FlagsMatrix] only. A question that is closed has no "Other answer" and no "Ask the agent".
 * - A request from an automatic system (`one_shot`, no conversation) has the header "Automatic system (no conversation)", its
 *   suggestion and the confidence above the options, and no row, no field and no link of a discussion.
 * - Send is off until every block has a valid answer ([AnswerSheet]). Send asks for the local biometric: the prompt is the last gate.
 *   The phone needs a screen lock or a biometric, as for an approval. Decline to answer needs no gate: it runs nothing.
 * - While an answer is on its way, both buttons are off.
 * - The server locks `accept` for the items of a new agent when `inbox.agents.lock_new` is true (SPEC 3.8.16). The phone then
 *   turns Send off and says why. Decline, and a question to the agent, stay on.
 */
object QuestionState {
    /**
     * The lock of a new agent applies to a question as well as to an approval: the server refuses `accept` for every item of the
     * agent (SPEC 3.8.16). Set to false only if the server stops locking questions.
     */
    const val LOCK_APPLIES_TO_QUESTIONS = true

    const val DECLINE_WORD = "Decline to answer"
    const val AUTOMATIC_HEADER = "Automatic system (no conversation)"
    const val SECRET_WARNING = "Do not enter a password or a secret. The agent reads this text."
    const val OTHER_WORD = "Other answer"
    const val OTHER_NOTE = "Your text is the answer. It ends the question."
    const val ASK_WORD = "Ask the agent a question"
    const val ASK_NOTE = "This is not an answer. The question stays open and the agent replies."
    const val DISCUSSION_WORD = "Open the discussion"
    const val NO_DISCUSSION = "This system cannot read free text. You can only choose or decline. There is no discussion."
    const val LOCK_HINT = "Send is off: this agent is new. Name it or allow it first. You can decline now."
    const val NO_ANSWER_NOTE = "This message takes no answer. You can close it with Decline to answer."

    enum class Mode {
        /** The owner can answer. */
        OPEN,

        /** An answer is on its way. */
        SENDING,

        /** The flags break a rule of the protocol ([FlagsMatrix.Invalid]). Only Decline works. */
        INVALID,

        /** The item is final. The screen is read only. */
        CLOSED,
    }

    /** Why Send is off. The first reason of this order wins. */
    enum class Lock { SENDING, PORTAL_BLOCKED, AGENT_LOCKED, NOT_COMPLETE, NO_SCREEN_LOCK }

    data class Input(
        val item: Item,
        val nowMs: Long,
        val draft: SheetDraft?,
        val answer: AnswerProgress,
        /** The phone can ask for a biometric or a device credential. */
        val canAuthenticate: Boolean,
        val link: ApprovalState.Link = ApprovalState.Link.ONLINE,
        val portal: String = "this portal",
        /** The agent is new and the portal locks the new agents. */
        val agentLocked: Boolean = false,
        /** The agent discussed this question already: the row says "Open the discussion". */
        val discussed: Boolean = false,
    )

    /** The suggestion of the agent or of a system. It never answers: the owner chooses. */
    data class HintBox(
        /** The label of the suggested option, or the suggestion as the agent wrote it. Escaped. */
        val suggested: String,
        /** 0 to 100, or null when the sender gave no confidence. */
        val percent: Int?,
        val source: String,
        val note: String,
        val spoken: String,
    )

    data class Screen(
        val mode: Mode,
        val automatic: Boolean,
        /** "Question" or "Decision". */
        val kindWord: String,
        /** "conversational agent" or "Automatic system (no conversation)". */
        val typeLabel: String,
        val senderLine: String,
        val statusLine: String,
        val statusSpoken: String,
        val hint: HintBox?,
        val rows: FlagsMatrix.Rows,
        val showOther: Boolean,
        val showAsk: Boolean,
        val askWord: String,
        val sendShown: Boolean,
        val sendLock: Lock?,
        val sendWord: String,
        val declineShown: Boolean,
        val declineEnabled: Boolean,
        /** The line above the bottom bar. */
        val bottomHint: String?,
        /** The line at the top for a closed item, or a result. */
        val notice: String?,
        /** The line about the link: offline or blocked. */
        val linkLine: String?,
        /** The line under the options of a one_shot request: no free text, no discussion. */
        val closedLine: String?,
        /** The answer goes to the outbox: the portal is offline. */
        val queued: Boolean,
    ) {
        val sendEnabled: Boolean get() = sendShown && sendLock == null
    }

    fun of(i: Input): Screen {
        val item = i.item
        val r = item.request
        val automatic = r.from.interaction == Interaction.ONE_SHOT
        val rows = FlagsMatrix.rows(r)
        val type = r.question?.input?.type
        val sheet: Sheet? = if (rows.invalid == null) AnswerSheet.of(r) else null
        val closed = item.kind != RequestKind.QUESTION || !item.isOpen
        val mode = when {
            closed -> Mode.CLOSED
            i.answer.pending -> Mode.SENDING
            rows.invalid != null -> Mode.INVALID
            else -> Mode.OPEN
        }
        val blocked = i.link == ApprovalState.Link.BLOCKED && mode != Mode.CLOSED
        val draft = i.draft ?: sheet?.let { AnswerSheet.empty(it) }
        val complete = sheet != null && draft != null && AnswerSheet.missing(sheet, draft).isEmpty()
        val sendShown = (mode == Mode.OPEN || mode == Mode.SENDING) && sheet != null
        val lock = when {
            mode == Mode.SENDING -> Lock.SENDING
            blocked -> Lock.PORTAL_BLOCKED
            i.agentLocked && LOCK_APPLIES_TO_QUESTIONS -> Lock.AGENT_LOCKED
            !complete -> Lock.NOT_COMPLETE
            !i.canAuthenticate -> Lock.NO_SCREEN_LOCK
            else -> null
        }
        val declineEnabled = (mode == Mode.OPEN || mode == Mode.INVALID) && !blocked
        val showAsk = mode == Mode.OPEN && rows.askAgent && !automatic
        val multi = (sheet?.blocks?.size ?: 0) > 1
        return Screen(
            mode = mode,
            automatic = automatic,
            kindWord = if (automatic) "Decision" else "Question",
            typeLabel = if (automatic) AUTOMATIC_HEADER else "conversational agent",
            senderLine = senderLine(item, automatic),
            statusLine = statusLine(item, automatic, i.nowMs, sheet, draft),
            statusSpoken = statusSpoken(item, automatic, i.nowMs, sheet, draft),
            hint = hint(r.question?.hint, sheet),
            rows = rows,
            showOther = mode != Mode.CLOSED && rows.otherAnswer,
            showAsk = showAsk,
            askWord = if (i.discussed) DISCUSSION_WORD else ASK_WORD,
            sendShown = sendShown,
            sendLock = if (sendShown) lock else null,
            sendWord = when {
                i.link == ApprovalState.Link.OFFLINE && mode == Mode.OPEN -> if (multi) "Queue answers" else "Queue answer"
                multi -> "Send answers"
                else -> "Send answer"
            },
            declineShown = mode != Mode.CLOSED,
            declineEnabled = declineEnabled,
            bottomHint = bottomHint(mode, if (sendShown) lock else null, i.answer, type, sheet, draft),
            notice = if (mode == Mode.INVALID) "Invalid request. " + (rows.invalid?.line ?: "") + " The phone does not answer it. You can decline it." else null,
            linkLine = if (mode == Mode.CLOSED) null else ApprovalState.offlineLine(i.link, i.portal),
            closedLine = if (automatic && mode != Mode.CLOSED) NO_DISCUSSION else null,
            queued = i.link == ApprovalState.Link.OFFLINE && mode == Mode.OPEN,
        )
    }

    /** The note under the row "Other answer": what the text does to the options. */
    fun otherNote(kind: BlockKind, isForm: Boolean): String = when {
        kind == BlockKind.CHECKBOX -> "Your own answer. It replaces the ticks."
        isForm -> "Your own answer. It replaces the choices."
        else -> OTHER_NOTE
    }

    /** "ops-agent · key of alice · conversational agent", or "Automatic system (no conversation) · jev · key of alice". */
    fun senderLine(item: Item, automatic: Boolean, label: String = item.request.from.label): String {
        val f = item.request.from
        val parts = if (automatic) listOf(AUTOMATIC_HEADER, label, "key of " + f.owner) else listOf(label, "key of " + f.owner, "conversational agent")
        return parts.joinToString(" · ") { VisibleText.escapeLine(it) }
    }

    private fun progress(sheet: Sheet?, draft: SheetDraft?): String? {
        if (sheet == null || draft == null || !sheet.isForm) return null
        val done = sheet.blocks.indices.count { AnswerSheet.complete(sheet, draft, it) }
        return "$done of ${sheet.blocks.size} answered"
    }

    /** "Asked 12 min ago · agent active 1 min ago", or for a system "Asked 12 min ago · last computed 12 min ago". */
    private fun statusLine(item: Item, automatic: Boolean, nowMs: Long, sheet: Sheet?, draft: SheetDraft?): String {
        val seen = if (automatic) "last computed " + Ages.ago(item.request.agentSeenAtMs, nowMs) else ItemTexts.agentActive(item, nowMs)
        return listOfNotNull(ItemTexts.asked(item, nowMs), seen, if (item.isOpen) progress(sheet, draft) else null).joinToString(" · ")
    }

    private fun statusSpoken(item: Item, automatic: Boolean, nowMs: Long, sheet: Sheet?, draft: SheetDraft?): String {
        val asked = "asked " + Ages.spoken(item.request.createdAtMs, nowMs)
        val seen = (if (automatic) "last computed " else "agent active ") + Ages.spoken(item.request.agentSeenAtMs, nowMs)
        return listOfNotNull(asked, seen, if (item.isOpen) progress(sheet, draft) else null).joinToString(", ")
    }

    /** The suggestion box. The suggested option shows by its label when the question has it. Nothing is ticked for the owner. */
    fun hint(h: Hint?, sheet: Sheet?): HintBox? {
        h ?: return null
        val label = sheet?.blocks?.singleOrNull()?.options?.firstOrNull { it.id == h.suggested }?.label ?: h.suggested
        val shown = VisibleText.escapeLine(label)
        val percent = h.confidence?.let { Math.round(it * 100).toInt() }
        val source = VisibleText.escapeLine(h.source)
        val spoken = "Suggestion from $source, not checked: $shown" + (percent?.let { ", confidence $it percent" } ?: "")
        return HintBox(shown, percent, source, "Not checked. The sender suggests this answer. You decide.", spoken)
    }

    private fun bottomHint(
        mode: Mode, lock: Lock?, answer: AnswerProgress, type: InputType?, sheet: Sheet?, draft: SheetDraft?,
    ): String? = when (mode) {
        Mode.CLOSED -> null
        Mode.INVALID -> "The phone does not answer this request. You can decline it."
        Mode.SENDING -> OutboxAnswer.pendingLine(answer)
        Mode.OPEN -> when {
            type == InputType.NONE -> NO_ANSWER_NOTE
            else -> when (lock) {
                Lock.PORTAL_BLOCKED -> "Nothing can be sent to this portal now."
                Lock.AGENT_LOCKED -> LOCK_HINT
                Lock.NOT_COMPLETE -> if (sheet != null && draft != null) AnswerSheet.missingLine(sheet, draft) else null
                Lock.NO_SCREEN_LOCK -> "Set a screen lock or a fingerprint on this phone to send an answer."
                Lock.SENDING -> null
                null -> "Send asks for your fingerprint or your device PIN."
            }
        }
    }
}
