package app.portalinbox.ui

import app.portalinbox.data.Item
import app.portalinbox.data.OutboxEntry
import app.portalinbox.data.OwnReply
import app.portalinbox.data.ReplyOutbox
import app.portalinbox.data.ReplyStatus
import app.portalinbox.data.Turn
import app.portalinbox.proto.Author
import app.portalinbox.proto.Command
import app.portalinbox.proto.ExplanationFormat
import app.portalinbox.proto.Interaction
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.RequestKind
import java.time.ZoneId

/**
 * What the discussion screen shows and lets the owner do (CONF 3.5, 3.6, DIAL 4.4, 4.5). Pure Kotlin: the Activity
 * gives the facts and draws the answer.
 *
 * Rules:
 * - The thread holds the turns of the server in the order of their `seq`, and the messages of the owner that this phone
 *   sent. The server does not send a turn back to the phone that wrote it, so these come from the outbox.
 * - The owner has one message at a time. The field is blocked while a message waits for its result, and while the agent
 *   prepares its reply (the item is `replied`).
 * - The server decides the state. The phone never closes a discussion by its own clock.
 * - A closed item (abandoned, replaced, expired, cancelled, answered) has no field. It shows how it ended, and what
 *   happened to a message that did not arrive.
 * - Every agent turn is a text of a third party: it shows with the label "Not checked".
 */
object ThreadState {
    data class Input(
        val item: Item,
        val turns: List<Turn>,
        val outbox: List<OutboxEntry>,
        /** The time of the server now ([app.portalinbox.data.ClockOffsets]). */
        val nowMs: Long,
        /** `server time - phone time`: puts the time of a message of the outbox on the clock of the server. */
        val offsetMs: Long = 0L,
        val zone: ZoneId = ZoneId.systemDefault(),
        /** The name of the agent from the table of the agents (lot G4), or null: then `from.label` of the frame is the name. */
        val agentName: String? = null,
    )

    enum class Delivery { DELIVERED, QUEUED, SENT, NOT_SENT }

    /** One bubble of the thread. */
    sealed interface Entry {
        val atMs: Long
        val whenText: String

        /** A turn of the agent. [explanation] and [command] are texts of a third party. */
        data class Agent(
            val seq: Long,
            override val atMs: Long,
            override val whenText: String,
            val explanation: String?,
            val format: ExplanationFormat?,
            val command: Command?,
        ) : Entry

        /** A message of the owner. [seq] is null for a message that the server did not number yet. */
        data class User(
            val seq: Long?,
            override val atMs: Long,
            override val whenText: String,
            val text: String,
            val delivery: Delivery,
            /** The small line under the bubble: waiting, or why it did not arrive. */
            val line: String?,
            val clientMsgId: String?,
        ) : Entry
    }

    enum class Phase { YOUR_TURN, WAITING_FOR_AGENT, CLOSED }

    /** Why the field is blocked. */
    enum class Block { NONE, WAITING_FOR_AGENT, SENDING, CLOSED, NOT_ALLOWED }

    data class Composer(
        val block: Block,
        val placeholder: String,
        /** The line above the field. */
        val hint: String?,
    ) {
        val enabled: Boolean get() = block == Block.NONE
    }

    enum class Tone { WARNING, INFO, MUTED }

    data class Banner(val text: String, val tone: Tone)

    data class Chip(val word: String, val tone: Tone)

    data class Screen(
        val title: String,
        /** "ops-agent · key of alice". */
        val senderLine: String,
        /** "agent active 4 min ago · your turn", or "" for a closed item. */
        val statusLine: String,
        /** The same line in full words for a screen reader: "agent active 4 minutes ago, your turn". */
        val statusSpoken: String,
        val chip: Chip?,
        val phase: Phase,
        /** The first bubble of a question: its text. Null for an approval (the request has its own screen). */
        val opening: Opening?,
        val entries: List<Entry>,
        /** The agent has the move: the screen shows "ops-agent is writing". */
        val writing: Boolean,
        val composer: Composer,
        val banner: Banner?,
        /** The request id of the item that replaced this one, or null. */
        val replacedBy: String?,
        val agentLabel: String,
    )

    /** The question that opens a thread. Its options show as chips that do not answer (the screen of questions is lot A4). */
    data class Opening(
        val whenText: String,
        val explanation: String?,
        val format: ExplanationFormat?,
        val command: Command?,
        val options: List<String>,
        val title: String,
    )

    /** The agent has the move: its answer to the owner is due. */
    fun waitingForAgent(item: Item, outbox: List<OutboxEntry>): Boolean =
        item.isOpen && (item.state == ItemState.REPLIED || ReplyOutbox.pending(outbox) != null)

    fun of(i: Input): Screen {
        val item = i.item
        val agent = VisibleText.escapeLine(i.agentName ?: item.request.from.label)
        val waiting = waitingForAgent(item, i.outbox)
        val allowed = item.request.allowAskBack && item.request.from.interaction != Interaction.ONE_SHOT
        val phase = when {
            !item.isOpen -> Phase.CLOSED
            waiting -> Phase.WAITING_FOR_AGENT
            else -> Phase.YOUR_TURN
        }
        val own = ReplyOutbox.replies(i.outbox)
        val entries = entries(i, own)
        val pending = ReplyOutbox.pending(i.outbox)
        val block = when {
            !item.isOpen -> Block.CLOSED
            !allowed -> Block.NOT_ALLOWED
            pending != null -> Block.SENDING
            waiting -> Block.WAITING_FOR_AGENT
            else -> Block.NONE
        }
        return Screen(
            title = ItemTexts.title(item),
            senderLine = ItemTexts.senderLine(item, i.agentName ?: item.request.from.label),
            statusLine = statusLine(item, phase, i.nowMs),
            statusSpoken = statusSpoken(item, phase, i.nowMs),
            chip = chipOf(item),
            phase = phase,
            opening = opening(item, i),
            entries = entries,
            writing = item.isOpen && waiting && pending == null,
            composer = composer(item, block, agent, pending),
            banner = banner(item, i.zone, i.nowMs),
            replacedBy = item.settled?.supersededBy?.takeIf { item.state == ItemState.SUPERSEDED },
            agentLabel = agent,
        )
    }

    private fun statusLine(item: Item, phase: Phase, nowMs: Long): String = when (phase) {
        Phase.CLOSED -> ""
        Phase.YOUR_TURN -> ItemTexts.agentActive(item, nowMs) + " · your turn"
        Phase.WAITING_FOR_AGENT -> ItemTexts.agentActive(item, nowMs) + " · waiting for the agent"
    }

    private fun statusSpoken(item: Item, phase: Phase, nowMs: Long): String {
        val active = "agent active " + Ages.spoken(item.request.agentSeenAtMs, nowMs)
        return when (phase) {
            Phase.CLOSED -> ""
            Phase.YOUR_TURN -> "$active, your turn"
            Phase.WAITING_FOR_AGENT -> "$active, waiting for the agent"
        }
    }

    private fun chipOf(item: Item): Chip? = when (item.state) {
        ItemState.PENDING, ItemState.REPLIED -> null
        ItemState.ABANDONED -> Chip("abandoned", Tone.WARNING)
        ItemState.SUPERSEDED -> Chip("replaced", Tone.INFO)
        else -> Chip(ItemTexts.stateWord(item), Tone.MUTED)
    }

    private fun opening(item: Item, i: Input): Opening? {
        val q = item.request.question ?: return null
        return Opening(
            whenText = Ages.stamp(item.request.createdAtMs, i.nowMs, i.zone),
            explanation = q.explanation,
            format = q.explanationFormat,
            command = q.command,
            options = q.input.options.map { VisibleText.escapeLine(it.label) },
            title = ItemTexts.title(item),
        )
    }

    private fun entries(i: Input, own: List<OwnReply>): List<Entry> {
        val serverTurns = i.turns.sortedBy { it.seq }.map { t ->
            val f = t.frame
            if (f.by == Author.AGENT) {
                Entry.Agent(f.seq, f.createdAtMs, Ages.stamp(f.createdAtMs, i.nowMs, i.zone), f.explanation, f.explanationFormat, f.command)
            } else {
                Entry.User(f.seq, f.createdAtMs, Ages.stamp(f.createdAtMs, i.nowMs, i.zone), f.text ?: "", Delivery.DELIVERED, null, null)
            }
        }
        val users = serverTurns.filterIsInstance<Entry.User>()
        val mine = own.mapNotNull { r ->
            val at = r.atMs + i.offsetMs
            // A copy of the same text from the server (a phone that did not know this one) is the same message.
            if (r.status == ReplyStatus.Delivered && users.any { it.text == r.text && kotlin.math.abs(it.atMs - at) <= DUPLICATE_WINDOW_MS }) {
                return@mapNotNull null
            }
            val delivery = when (r.status) {
                ReplyStatus.Queued -> Delivery.QUEUED
                ReplyStatus.Sent -> Delivery.SENT
                ReplyStatus.Delivered -> Delivery.DELIVERED
                is ReplyStatus.Rejected -> Delivery.NOT_SENT
            }
            Entry.User(null, at, Ages.stamp(at, i.nowMs, i.zone), r.text, delivery, ReplyOutbox.line(r.status), r.clientMsgId)
        }
        // By time. At the same time, the turns of the server come in the order of their `seq`, and then the messages of this phone.
        return (serverTurns + mine).sortedWith(
            compareBy<Entry> { it.atMs }.thenBy { if (it is Entry.Agent) it.seq else if (it is Entry.User) it.seq ?: Long.MAX_VALUE else 0L },
        )
    }

    /** Two copies of one message are at most this far apart. */
    const val DUPLICATE_WINDOW_MS = 120_000L

    private fun composer(item: Item, block: Block, agent: String, pending: OwnReply?): Composer = when (block) {
        Block.NONE -> Composer(
            block, "Write to the agent",
            if (item.kind == RequestKind.APPROVAL) "Write to the agent. Approve or decline on the request screen." else "Write to the agent.",
        )
        Block.WAITING_FOR_AGENT -> Composer(block, "The agent is writing", "The agent is writing its answer. You can write again when it replies.")
        Block.SENDING -> Composer(
            block, "Sending your message",
            if (pending?.status == ReplyStatus.Sent) "Your message is sent. Waiting for the server." else "Your message is saved. It goes out when the phone is online.",
        )
        Block.NOT_ALLOWED -> Composer(block, "The agent does not take questions here", "$agent does not take questions here.")
        Block.CLOSED -> Composer(block, "This discussion is closed", null)
    }

    private fun banner(item: Item, zone: ZoneId, nowMs: Long): Banner? {
        val noun = if (item.kind == RequestKind.APPROVAL) "request" else "question"
        return when (item.state) {
            ItemState.PENDING, ItemState.REPLIED -> null
            ItemState.ABANDONED -> {
                val seen = item.request.agentSeenAtMs
                Banner(
                    "The agent no longer answers. Last sign ${Ages.ago(seen, nowMs)} (${Ages.dayTime(seen, zone)}). " +
                        "This is not a decline. You cannot write now.",
                    Tone.WARNING,
                )
            }
            ItemState.SUPERSEDED -> Banner("The agent proposes something else. This $noun is closed. A new $noun replaces it.", Tone.INFO)
            ItemState.EXPIRED -> Banner("${ItemTexts.reasonLine(item)}. No decision was made. The $noun is closed.", Tone.MUTED)
            ItemState.CANCELLED -> Banner("The agent cancelled this $noun. The discussion is closed.", Tone.MUTED)
            ItemState.ANSWERED -> Banner((ItemTexts.closedNotice(item) ?: "Answered.") + " The discussion is closed.", Tone.MUTED)
        }
    }
}
