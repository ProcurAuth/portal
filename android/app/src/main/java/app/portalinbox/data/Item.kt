package app.portalinbox.data

import app.portalinbox.core.ResolvedView
import app.portalinbox.proto.AgentResultFrame
import app.portalinbox.proto.AnswerResultFrame
import app.portalinbox.proto.ClientFrame
import app.portalinbox.proto.ClientFrames
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.MessageFrame
import app.portalinbox.proto.Prefs
import app.portalinbox.proto.RequestFrame
import app.portalinbox.proto.RequestKind
import app.portalinbox.proto.SettledFrame

/**
 * What the phone knows about the request of an approval after it computed the effect again.
 * Only `VALID` lets the owner approve. Any other value shows "Invalid request".
 */
enum class Validity(val wire: String) {
    VALID("valid"),

    /** The hash that the phone computed from `op` is not the `effect` of the frame. */
    EFFECT_MISMATCH("effect_mismatch"),

    /** `op` breaks the rules of the effect (`invalid_op`). */
    INVALID_OP("invalid_op"),

    /** A pointer of the view names no value of `op`, or the view breaks a rule (`invalid_view`). */
    INVALID_VIEW("invalid_view");

    val isValid: Boolean get() = this == VALID

    companion object {
        fun of(wire: String): Validity = entries.firstOrNull { it.wire == wire } ?: EFFECT_MISMATCH
    }
}

/**
 * One item of a server: a question or an approval, as the last `request` frame described it, plus
 * what this phone computed and knows. The request frame is the local copy of the body: the app
 * keeps it for the history, whatever the server purges.
 */
data class Item(
    val serverId: Long,
    val request: RequestFrame,
    val state: ItemState,
    val validity: Validity,
    /** The hash that the phone computed from the `op` that it received. Null for a question. */
    val computedEffect: String?,
    /** The view with the values of `op` (the command). Null for a question or an invalid request. */
    val view: ResolvedView?,
    /** The frame that made the item final. Null while the item is open. */
    val settled: SettledFrame?,
    val receivedAtMs: Long,
    val updatedAtMs: Long,
    /** True when the phone already showed a notification for the item. */
    val notified: Boolean,
) {
    val requestId: String get() = request.requestId
    val kind: RequestKind get() = request.kind
    val isOpen: Boolean get() = state.isOpen

    /** The owner can approve: an open approval whose effect the phone verified. */
    val canApprove: Boolean get() = kind == RequestKind.APPROVAL && isOpen && validity.isValid

    /** The command of the item as text: the body of the view of an approval, or the command of a question. */
    val commandText: String? get() = view?.body ?: request.question?.command?.text
}

/** A turn of the thread of an item, as the server sent it. */
data class Turn(val serverId: Long, val frame: MessageFrame, val receivedAtMs: Long) {
    val requestId: String get() = frame.requestId
    val seq: Long get() = frame.seq
}

enum class OutboxType(val wire: String) {
    ANSWER("answer"), REPLY("reply"),

    /** An action of the owner on an agent (lot G4). Its `request_id` column holds the id of the agent. */
    AGENT_DO("agent_do");

    companion object {
        fun of(wire: String): OutboxType = entries.first { it.wire == wire }
    }
}

/**
 * A frame that the owner made and the server did not confirm. The phone sends it again after each
 * reconnection until the `answer_result` arrives, or until a `settled` frame closes the item.
 */
data class OutboxEntry(
    val serverId: Long,
    val clientMsgId: String,
    val requestId: String,
    val type: OutboxType,
    /** The frame as JSON text. */
    val frame: String,
    val createdAtMs: Long,
    val attempts: Int,
    val lastSentAtMs: Long?,
    /** The `status` of the `answer_result`, or `settled`. Null while the entry waits. */
    val result: String?,
    val closedAtMs: Long?,
) {
    val isPending: Boolean get() = closedAtMs == null

    fun toFrame(): ClientFrame = ClientFrames.decode(frame)
}

/** The preferences that the last `welcome` gave, and the age at which the server erases bodies. */
data class StoredPrefs(val prefs: Prefs, val bodyPurgeAfterMs: Long)

/** What happened to the items after a frame. The caller shows it: notification, screen, withdrawal. */
sealed interface ItemEvent {
    /** A new open item. The caller can notify. */
    data class NewRequest(val item: Item) : ItemEvent

    /** The agent revived an abandoned item. */
    data class Revived(val item: Item) : ItemEvent

    /** A newer `request` frame changed an open item. */
    data class Updated(val item: Item) : ItemEvent

    data class AgentMessage(val serverId: Long, val requestId: String, val turn: Turn) : ItemEvent

    data class UserMessage(val serverId: Long, val requestId: String, val turn: Turn) : ItemEvent

    /** The item is final: withdraw its notification. */
    data class Settled(val item: Item) : ItemEvent

    /** A digest of the open items of a portal. [deliveryId] is the `id` of the frame: a new digest has a new one. */
    data class Digest(val serverId: Long, val deliveryId: String, val count: Long, val oldestCreatedAtMs: Long, val ids: List<String>) : ItemEvent

    data class AnswerResult(val serverId: Long, val result: AnswerResultFrame, val entry: OutboxEntry) : ItemEvent

    /** The phone did not hold this agent, and it is `new`. The caller counts it in the one notification, once. */
    data class NewAgent(val agent: Agent) : ItemEvent

    /** A frame with a higher `rev` changed an agent that the phone held. */
    data class AgentChanged(val agent: Agent) : ItemEvent

    /** The result of an action of the owner on an agent. [entry] is the outbox entry that it closed, if the phone knew it. */
    data class AgentResult(val serverId: Long, val result: AgentResultFrame, val entry: OutboxEntry?) : ItemEvent
}

/** The result of a frame: the delivery to acknowledge (after the durable write) and the events. */
data class Outcome(val ack: String?, val events: List<ItemEvent>) {
    companion object {
        val NONE = Outcome(null, emptyList())
    }
}

/** What the phone sends after a `welcome`: the `sync` frame and the frames of the outbox. */
data class SessionStart(val sync: ClientFrame?, val outbox: List<OutboxEntry>)

/** Why the engine did not make a frame. */
enum class Refusal {
    NOT_FOUND, NOT_AN_APPROVAL, NOT_OPEN,

    /** An answer of a question for an item that is not a question, or a question that takes no answer on a sheet. */
    NOT_A_QUESTION,

    /** The request is not valid: the phone never approves it. */
    INVALID_REQUEST,

    /** An answer of this item waits in the outbox. */
    ALREADY_QUEUED,
    CUSTOM_NOT_ALLOWED, ASK_BACK_NOT_ALLOWED,

    /** The text or the combination breaks a rule of the frame. */
    BAD_INPUT,

    /** An action on an agent: the phone does not hold the agent, or the action does not fit its state. */
    BAD_STATE,
}

sealed interface Submit {
    data class Queued(val entry: OutboxEntry) : Submit
    data class Refused(val reason: Refusal) : Submit
}
