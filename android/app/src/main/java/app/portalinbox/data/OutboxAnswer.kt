package app.portalinbox.data

import app.portalinbox.proto.Action
import app.portalinbox.proto.AnswerFrame
import app.portalinbox.proto.AnswerStatus
import app.portalinbox.proto.ReplyFrame

/** Where the answer of the owner is, from the outbox entries of one item. */
sealed interface AnswerProgress {
    /** No answer was made, or the last one is not an answer (a reply). */
    data object None : AnswerProgress

    /** The answer waits in the outbox. No session sent it yet. */
    data class Queued(val action: Action) : AnswerProgress

    /** A session sent the answer. The result did not come. */
    data class Sent(val action: Action) : AnswerProgress

    /** The server gave a result. [status] is its `status`, or `settled` text when an item became final first. */
    data class Done(val action: Action, val result: String) : AnswerProgress {
        val status: AnswerStatus? get() = AnswerStatus.entries.firstOrNull { it.wire == result }
    }

    val pending: Boolean get() = this is Queued || this is Sent
}

/**
 * What the screen says about the answer of an item. Pure Kotlin. The words are English, short and plain.
 * The caller reads the outbox entries of the item (`ItemRepository.outboxOf`) and gives them here.
 */
object OutboxAnswer {
    /** The progress of the last answer of the entries (a reply is not an answer). */
    fun progress(entries: List<OutboxEntry>): AnswerProgress {
        val e = entries.filter { it.type == OutboxType.ANSWER }.maxByOrNull { it.createdAtMs } ?: return AnswerProgress.None
        val action = actionOf(e) ?: return AnswerProgress.None
        return when {
            e.isPending && e.lastSentAtMs == null -> AnswerProgress.Queued(action)
            e.isPending -> AnswerProgress.Sent(action)
            else -> AnswerProgress.Done(action, e.result ?: ItemEngine.RESULT_SETTLED)
        }
    }

    private fun actionOf(e: OutboxEntry): Action? = (runCatching { e.toFrame() }.getOrNull() as? AnswerFrame)?.action

    /** The line under the buttons while the answer is on its way, or null. */
    fun pendingLine(p: AnswerProgress): String? = when (p) {
        is AnswerProgress.Queued -> "Your answer is saved. It goes out when the phone is online."
        is AnswerProgress.Sent -> "Your answer is sent. Waiting for the server."
        else -> null
    }

    /**
     * The message for a result, or null when nothing needs to be said. [tooLate] results use the words
     * "Too late" (mock 6b): the item ended before the answer arrived.
     */
    fun resultLine(p: AnswerProgress.Done): String? = when (p.result) {
        AnswerStatus.ACCEPTED.wire -> if (p.action == Action.ACCEPT) "You approved this request." else "You declined this request."
        AnswerStatus.ALREADY_ANSWERED.wire -> "Already answered on another device."
        AnswerStatus.ABANDONED.wire -> "Too late: the agent no longer answers."
        AnswerStatus.SUPERSEDED.wire -> "Too late: a newer request replaced this one."
        AnswerStatus.EXPIRED.wire -> "Too late: this request expired."
        AnswerStatus.CANCELLED.wire -> "Too late: the agent cancelled this request."
        AnswerStatus.CLOSED.wire -> "This request is closed."
        AnswerStatus.EFFECT_MISMATCH.wire -> "The command changed. Open the request again and read it."
        AnswerStatus.DEVICE_REVOKED.wire -> "This phone is no longer allowed to answer. Add the server again."
        AnswerStatus.CUSTOM_NOT_ALLOWED.wire -> "The agent does not take a text with a decline."
        AnswerStatus.ASK_BACK_NOT_ALLOWED.wire -> "The agent does not take questions here."
        AnswerStatus.AGENT_LOCKED.wire -> "This agent is new and locked. Allow the agent first, then approve."
        ItemEngine.RESULT_SETTLED -> null
        else -> null
    }

    /** True for a result that ended the try without an answer: the owner can answer again. */
    fun canTryAgain(p: AnswerProgress.Done): Boolean = when (p.status) {
        AnswerStatus.CUSTOM_NOT_ALLOWED, AnswerStatus.ASK_BACK_NOT_ALLOWED, AnswerStatus.DEVICE_REVOKED,
        AnswerStatus.AGENT_LOCKED -> true
        else -> false
    }

    /**
     * Records an answer in the outbox and says if the caller must flush it. Approve and decline of the engine
     * give a [Submit]: this turns it into the message for the owner.
     */
    fun submitLine(s: Submit): String? = when (s) {
        is Submit.Queued -> null
        is Submit.Refused -> when (s.reason) {
            Refusal.NOT_FOUND -> "This request is gone."
            Refusal.NOT_AN_APPROVAL -> "This is not an approval."
            Refusal.NOT_OPEN -> "This request is closed."
            Refusal.INVALID_REQUEST -> "Invalid request. The phone does not approve it."
            Refusal.ALREADY_QUEUED -> "An answer is already on its way."
            Refusal.CUSTOM_NOT_ALLOWED -> "The agent does not take a text with a decline."
            Refusal.ASK_BACK_NOT_ALLOWED -> "The agent does not take questions here."
            Refusal.BAD_INPUT -> "The text is not valid. Check it and try again."
            Refusal.BAD_STATE -> "This cannot be done now. The agent changed. Look at it again."
        }
    }

    /** The text of a queued reply entry, if any (used by lot A3). */
    fun replyText(e: OutboxEntry): String? = (runCatching { e.toFrame() }.getOrNull() as? ReplyFrame)?.text

    fun declineText(e: OutboxEntry): String? = (runCatching { e.toFrame() }.getOrNull() as? AnswerFrame)
        ?.takeIf { it.action == Action.DECLINE }?.text
}
