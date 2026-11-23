package app.portalinbox.ui

import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.BlockKind
import app.portalinbox.data.Item
import app.portalinbox.data.OutboxEntry
import app.portalinbox.data.OutboxType
import app.portalinbox.data.Sheet
import app.portalinbox.proto.Action
import app.portalinbox.proto.AnswerFrame
import app.portalinbox.proto.AnswerStatus
import app.portalinbox.proto.Channel
import app.portalinbox.proto.InputOption
import app.portalinbox.proto.ItemState
import java.time.ZoneId

/**
 * How a question ended, in words. Pure Kotlin.
 *
 * The `settled` frame of a question does not say what the answer was, nor that the owner declined: the server keeps the text
 * of an answer sealed, and `decision` is for approvals. The phone knows the answer that it sent itself (the frame in its outbox).
 * For an answer from the web or from another phone it says only who answered.
 */
object QuestionHistory {
    /** The answer that this phone sent and the server took. [atMs] is the clock of the phone. */
    data class Own(val frame: AnswerFrame, val atMs: Long) {
        val declined: Boolean get() = frame.action == Action.DECLINE
    }

    fun own(entries: List<OutboxEntry>): Own? {
        val e = entries.filter { it.type == OutboxType.ANSWER && it.result == AnswerStatus.ACCEPTED.wire }
            .maxByOrNull { it.createdAtMs } ?: return null
        val f = runCatching { e.toFrame() }.getOrNull() as? AnswerFrame ?: return null
        return Own(f, e.createdAtMs)
    }

    /** The word of the chip: `answered`, `declined`, `abandoned`... */
    fun word(item: Item, own: Own?): String = when {
        // `decision` of a `settled` frame is for approvals: a question that was answered is `answered`.
        item.state == ItemState.ANSWERED -> if (own?.declined == true) "declined" else "answered"
        else -> ItemTexts.stateWord(item)
    }

    private const val SHORT = 60

    private fun shorten(text: String): String {
        val oneLine = text.replace('\n', ' ')
        return if (oneLine.codePointCount(0, oneLine.length) <= SHORT) oneLine
        else oneLine.substring(0, oneLine.offsetByCodePoints(0, SHORT)) + "…"
    }

    /** One line that says what the owner answered: "Deploy now", "“not before 18:00”", "3 questions answered", "Declined". */
    fun summary(sheet: Sheet?, own: Own): String {
        if (own.declined) return "Declined"
        val blocks = answerBlocks(sheet, own.frame)
        if (sheet == null || blocks.isEmpty()) return "Answered"
        if (sheet.isForm) return "${blocks.size} questions answered"
        val b = blocks[0]
        return b.free?.let { "“" + VisibleText.escapeLine(shorten(it)) + "”" } ?: b.chosen.joinToString(", ") { VisibleText.escapeLine(it.label) }
    }

    /** The second line of a row of the list "Done", and the first words of a screen. */
    fun reasonLine(item: Item, own: Own?, sheet: Sheet?): String {
        if (item.state != ItemState.ANSWERED) return ItemTexts.reasonLine(item)
        if (own != null) return if (own.declined) "Declined by you" else "Answered: " + summary(sheet, own) + " · by you"
        val by = item.settled?.by ?: return "Answered"
        return when (by.via) {
            Channel.PHONE -> "Answered on " + (by.deviceName?.let { VisibleText.escapeLine(it) } ?: "a phone")
            Channel.WEB -> "Answered on the web"
        }
    }

    /** What the owner chose or wrote in one block. [free] is a free answer ("Other"), and then [chosen] is empty. */
    data class BlockAnswer(
        val index: Int,
        val header: String?,
        val question: String?,
        val chosen: List<InputOption>,
        val notChosen: List<InputOption>,
        val free: String?,
        val kind: BlockKind,
    )

    /** The blocks of the answer, in the order of the sheet. An answer that does not fit the sheet gives an empty list. */
    fun answerBlocks(sheet: Sheet?, f: AnswerFrame): List<BlockAnswer> {
        sheet ?: return emptyList()
        if (f.action != Action.ACCEPT) return emptyList()
        val parts: List<Pair<List<String>?, String?>> = if (sheet.isForm) {
            val answers = f.answers ?: return emptyList()
            sheet.blocks.map { b -> answers.firstOrNull { it.questionId == b.id }?.let { it.choice to it.text } ?: return emptyList() }
        } else {
            listOf(f.choice to f.text)
        }
        return sheet.blocks.mapIndexed { i, b ->
            val (choice, text) = parts[i]
            val chosen = b.options.filter { choice?.contains(it.id) == true }
            BlockAnswer(
                i, b.header, b.text, chosen,
                if (b.kind == BlockKind.CHECKBOX) b.options - chosen.toSet() else emptyList(),
                text, b.kind,
            )
        }
    }

    // ------------------------------------------------------------ the closed screen

    /** The banner of a closed question, or null while it is open. */
    fun banner(item: Item, own: Own?, zone: ZoneId, nowMs: Long): ThreadState.Banner? = when (item.state) {
        ItemState.PENDING, ItemState.REPLIED -> null
        ItemState.ANSWERED -> ThreadState.Banner(answeredLine(item, own, zone, nowMs), ThreadState.Tone.MUTED)
        ItemState.ABANDONED -> {
            val seen = item.request.agentSeenAtMs
            ThreadState.Banner(
                "The agent no longer answers. Last sign ${Ages.ago(seen, nowMs)} (${Ages.dayTime(seen, zone)}). " +
                    "This is not a decline. You cannot answer now.",
                ThreadState.Tone.WARNING,
            )
        }
        ItemState.SUPERSEDED -> ThreadState.Banner(
            "The agent proposes something else. This question is closed. A new question replaces it.", ThreadState.Tone.INFO,
        )
        ItemState.EXPIRED -> ThreadState.Banner("${ItemTexts.reasonLine(item)}. No decision was made. The question is closed.", ThreadState.Tone.MUTED)
        ItemState.CANCELLED -> ThreadState.Banner("The agent cancelled this question.", ThreadState.Tone.MUTED)
    }

    private fun answeredLine(item: Item, own: Own?, zone: ZoneId, nowMs: Long): String {
        if (own != null) {
            val at = Ages.stamp(own.atMs, nowMs, zone)
            return if (own.declined) "You declined to answer at $at. You cannot change this." else "You answered at $at. You cannot change your answer."
        }
        return reasonLine(item, null, null) + ". The answer is not shown on this phone."
    }

    /** The line for the late answer, and the other results. Null when nothing needs to be said. */
    fun resultLine(p: AnswerProgress.Done): String? = when (p.result) {
        AnswerStatus.ACCEPTED.wire -> if (p.action == Action.ACCEPT) "Your answer was sent." else "You declined to answer."
        AnswerStatus.ALREADY_ANSWERED.wire -> "Already answered on another device."
        AnswerStatus.ABANDONED.wire -> "Too late: the agent no longer answers. Your answer was not used."
        AnswerStatus.SUPERSEDED.wire -> "Too late: a newer question replaced this one. Your answer was not used."
        AnswerStatus.EXPIRED.wire -> "Too late: this question expired. Your answer was not used."
        AnswerStatus.CANCELLED.wire -> "Too late: the agent cancelled this question. Your answer was not used."
        AnswerStatus.CLOSED.wire -> "This question is closed."
        AnswerStatus.DEVICE_REVOKED.wire -> "This phone is no longer allowed to answer. Add the server again."
        AnswerStatus.CUSTOM_NOT_ALLOWED.wire -> "The agent does not take another answer here. Choose one of the options."
        AnswerStatus.ASK_BACK_NOT_ALLOWED.wire -> "The agent does not take questions here."
        AnswerStatus.AGENT_LOCKED.wire -> "This agent is new and locked. Allow the agent first, then answer."
        else -> null
    }

    /** The question as a title for the prompt: one line, at most 80 characters. */
    fun promptTitle(title: String): String {
        val line = VisibleText.escapeLine(title)
        return if (line.codePointCount(0, line.length) <= 80) line else line.substring(0, line.offsetByCodePoints(0, 79)) + "…"
    }
}
