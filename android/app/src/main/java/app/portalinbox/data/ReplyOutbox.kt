package app.portalinbox.data

import app.portalinbox.proto.AnswerStatus
import app.portalinbox.proto.MAX_USER_TEXT_CHARS
import app.portalinbox.proto.ReplyFrame

/** Where one message of the owner to the agent is. */
sealed interface ReplyStatus {
    /** The message waits in the outbox. No session sent it yet. */
    data object Queued : ReplyStatus

    /** A session sent the message. The result did not come. */
    data object Sent : ReplyStatus

    /** The server took the message (`accepted`). */
    data object Delivered : ReplyStatus

    /** The server did not take it, or an end of the item came first. [result] is the `status`, or `settled`. */
    data class Rejected(val result: String) : ReplyStatus

    val pending: Boolean get() = this is Queued || this is Sent
}

/** A message of the owner to the agent, from the outbox of the phone. */
data class OwnReply(val clientMsgId: String, val text: String, val atMs: Long, val status: ReplyStatus)

/**
 * The messages of the owner to the agent (`reply` frames). Pure Kotlin. The words are English, short and plain.
 *
 * A `reply` goes through the outbox like an answer: the phone saves it, sends it after each `welcome`, and keeps it
 * until the `answer_result` arrives. The server does not send the turn back to the phone that wrote it, so the
 * thread of the screen shows the messages of the outbox next to the turns of the server.
 */
object ReplyOutbox {
    const val MAX_CHARS = MAX_USER_TEXT_CHARS

    sealed interface Draft {
        data class Ok(val text: String) : Draft

        data class Refused(val reason: String) : Draft
    }

    /**
     * Checks the text of the field. A text of only spaces is no text. The text is not changed: the owner wrote it and
     * the agent reads it as it is.
     */
    fun check(text: String): Draft = when {
        text.isBlank() -> Draft.Refused("Write a message first.")
        text.contains('\u0000') -> Draft.Refused("The text has a character that is not allowed.")
        text.codePointCount(0, text.length) > MAX_CHARS -> Draft.Refused("The message is too long. Use $MAX_CHARS characters at most.")
        else -> Draft.Ok(text)
    }

    fun statusOf(e: OutboxEntry): ReplyStatus = when {
        e.isPending && e.lastSentAtMs == null -> ReplyStatus.Queued
        e.isPending -> ReplyStatus.Sent
        e.result == AnswerStatus.ACCEPTED.wire -> ReplyStatus.Delivered
        else -> ReplyStatus.Rejected(e.result ?: ItemEngine.RESULT_SETTLED)
    }

    /** The messages of the entries, the oldest first. Entries of other types are not messages. */
    fun replies(entries: List<OutboxEntry>): List<OwnReply> = entries
        .filter { it.type == OutboxType.REPLY }
        .sortedBy { it.createdAtMs }
        .mapNotNull { e ->
            val text = (runCatching { e.toFrame() }.getOrNull() as? ReplyFrame)?.text ?: return@mapNotNull null
            OwnReply(e.clientMsgId, text, e.createdAtMs, statusOf(e))
        }

    /** The message that waits for its result, or null. The owner has one message at a time. */
    fun pending(entries: List<OutboxEntry>): OwnReply? = replies(entries).lastOrNull { it.status.pending }

    /** The small line under a message of the owner, or null when nothing needs to be said. */
    fun line(status: ReplyStatus): String? = when (status) {
        ReplyStatus.Queued -> "Saved. It goes out when the phone is online."
        ReplyStatus.Sent -> "Sent. Waiting for the server."
        ReplyStatus.Delivered -> null
        is ReplyStatus.Rejected -> rejectedLine(status.result)
    }

    private fun rejectedLine(result: String): String = when (result) {
        AnswerStatus.ABANDONED.wire -> "Not sent. Too late: the agent no longer answers."
        AnswerStatus.SUPERSEDED.wire -> "Not sent. Too late: a newer request replaced this one."
        AnswerStatus.EXPIRED.wire -> "Not sent. Too late: this request expired."
        AnswerStatus.CANCELLED.wire -> "Not sent. Too late: the agent cancelled this request."
        AnswerStatus.CLOSED.wire -> "Not sent. This request is closed."
        AnswerStatus.ALREADY_ANSWERED.wire -> "Not sent. The request was already answered."
        AnswerStatus.ASK_BACK_NOT_ALLOWED.wire -> "Not sent. The agent does not take questions here."
        AnswerStatus.DEVICE_REVOKED.wire -> "Not sent. This phone is no longer allowed to answer. Add the server again."
        ItemEngine.RESULT_SETTLED -> "Not sent. The request ended first."
        else -> "Not sent."
    }

    /** The message for the owner when the engine refuses to make the frame, or null after a success. */
    fun submitLine(s: Submit): String? = when (s) {
        is Submit.Queued -> null
        is Submit.Refused -> when (s.reason) {
            Refusal.NOT_FOUND -> "This request is gone."
            Refusal.NOT_OPEN -> "This request is closed."
            Refusal.ALREADY_QUEUED -> "Your last message is still on its way."
            Refusal.ASK_BACK_NOT_ALLOWED -> "The agent does not take questions here."
            Refusal.BAD_INPUT -> "The text is not valid. Check it and try again."
            Refusal.NOT_AN_APPROVAL, Refusal.NOT_A_QUESTION, Refusal.INVALID_REQUEST, Refusal.CUSTOM_NOT_ALLOWED, Refusal.BAD_STATE -> "The message was not sent."
        }
    }
}
