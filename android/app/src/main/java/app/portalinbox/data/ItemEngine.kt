package app.portalinbox.data

import app.portalinbox.core.EffectHasher
import app.portalinbox.proto.Action
import app.portalinbox.proto.AnswerFrame
import app.portalinbox.proto.AnswerResultFrame
import app.portalinbox.proto.AnswerStatus
import app.portalinbox.proto.Author
import app.portalinbox.proto.ClientFrame
import app.portalinbox.proto.ClientFrames
import app.portalinbox.proto.DigestFrame
import app.portalinbox.proto.FrameException
import app.portalinbox.proto.Intent
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.MAX_SYNC_ENTRIES
import app.portalinbox.proto.MessageFrame
import app.portalinbox.proto.OpenRef
import app.portalinbox.proto.ReplyFrame
import app.portalinbox.proto.RequestFrame
import app.portalinbox.proto.RequestKind
import app.portalinbox.proto.ServerFrame
import app.portalinbox.proto.SettledFrame
import app.portalinbox.proto.SyncFrame
import app.portalinbox.proto.WelcomeFrame
import java.util.UUID

/**
 * The state of the items of the phone. Pure Kotlin: no Context, no clock of its own, no network.
 * It reads the frames of the server, keeps the items, the turns and the outbox in an
 * [ItemRepository], and tells the caller what to acknowledge, what to show and what to send.
 *
 * Rules:
 * - A frame is idempotent. The same delivery id twice changes nothing and is acknowledged again.
 * - The server decides the state. The phone never settles an item by its own clock.
 * - A `settled` frame that comes before its `request` is kept and applies when the request arrives.
 * - The phone computes the effect of each approval again ([ShownEffect]) and never approves an invalid one.
 * - An answer or a reply waits in the outbox until its `answer_result` arrives or the item is settled.
 *
 * The caller sends the receipt (`Outcome.ack`) only after the call returns: the write is durable then.
 */
class ItemEngine(
    private val repo: ItemRepository,
    private val hasher: EffectHasher,
    private val clock: () -> Long = System::currentTimeMillis,
    private val newMsgId: () -> String = { "m-" + UUID.randomUUID().toString().replace("-", "") },
) {
    /** Applies a frame of the server that concerns items. Other frames give [Outcome.NONE]. */
    fun onFrame(serverId: Long, frame: ServerFrame): Outcome = when (frame) {
        is RequestFrame -> onRequest(serverId, frame)
        is MessageFrame -> onMessage(serverId, frame)
        is SettledFrame -> onSettled(serverId, frame)
        is DigestFrame -> onDigest(serverId, frame)
        is AnswerResultFrame -> onAnswerResult(serverId, frame)
        else -> Outcome.NONE
    }

    // ------------------------------------------------------------ frames of the server

    fun onRequest(serverId: Long, f: RequestFrame): Outcome = repo.transaction {
        if (repo.hasDelivery(serverId, f.id)) return@transaction Outcome(f.id, emptyList())
        val now = clock()
        repo.addDelivery(serverId, f.id, now)
        val existing = repo.item(serverId, f.requestId)
        val events = ArrayList<ItemEvent>(1)
        when {
            existing == null -> {
                var item = build(serverId, f, ItemState.PENDING, now, now, false)
                val early = repo.takeEarlySettlement(serverId, f.requestId)
                if (early != null) {
                    item = item.copy(state = early.state, settled = early)
                    repo.closeOutboxOf(serverId, f.requestId, RESULT_SETTLED, now)
                } else {
                    events += ItemEvent.NewRequest(item)
                }
                repo.putItem(item)
            }
            f.seq > existing.request.seq && (existing.isOpen || existing.state == ItemState.ABANDONED) -> {
                val revived = existing.state == ItemState.ABANDONED
                val item = build(serverId, f, if (revived) ItemState.PENDING else existing.state, existing.receivedAtMs, now, existing.notified)
                repo.putItem(item)
                events += if (revived) ItemEvent.Revived(item) else ItemEvent.Updated(item)
            }
            // The same state again, an older state, or an item that is final: nothing changes.
        }
        Outcome(f.id, events)
    }

    private fun build(serverId: Long, f: RequestFrame, state: ItemState, receivedAtMs: Long, now: Long, notified: Boolean): Item {
        val approval = f.approval
        if (approval == null) {
            return Item(serverId, f, state, Validity.VALID, null, null, null, receivedAtMs, now, notified)
        }
        return when (val v = ShownEffect.verify(approval, hasher)) {
            is ShownEffect.Verdict.Valid ->
                Item(serverId, f, state, Validity.VALID, v.effect, v.view, null, receivedAtMs, now, notified)
            is ShownEffect.Verdict.Invalid ->
                Item(serverId, f, state, v.validity, null, null, null, receivedAtMs, now, notified)
        }
    }

    fun onMessage(serverId: Long, f: MessageFrame): Outcome = repo.transaction {
        if (repo.hasDelivery(serverId, f.id)) return@transaction Outcome(f.id, emptyList())
        val now = clock()
        repo.addDelivery(serverId, f.id, now)
        val turn = Turn(serverId, f, now)
        val inserted = repo.putTurn(turn)
        val events = ArrayList<ItemEvent>(1)
        if (inserted) {
            val item = repo.item(serverId, f.requestId)
            if (item != null && item.isOpen) {
                // The last turn tells whose move it is: the agent answered, or the owner asked.
                val last = repo.turns(serverId, f.requestId).maxByOrNull { it.seq }
                val state = if (last?.frame?.by == Author.USER) ItemState.REPLIED else ItemState.PENDING
                if (state != item.state) repo.putItem(item.copy(state = state, updatedAtMs = now))
            }
            events += if (f.by == Author.AGENT) ItemEvent.AgentMessage(serverId, f.requestId, turn)
            else ItemEvent.UserMessage(serverId, f.requestId, turn)
        }
        Outcome(f.id, events)
    }

    fun onSettled(serverId: Long, f: SettledFrame): Outcome = repo.transaction {
        if (repo.hasDelivery(serverId, f.id)) return@transaction Outcome(f.id, emptyList())
        val now = clock()
        repo.addDelivery(serverId, f.id, now)
        val item = repo.item(serverId, f.requestId)
        val events = ArrayList<ItemEvent>(1)
        if (item == null) {
            repo.putEarlySettlement(serverId, f)
        } else {
            val settled = item.copy(state = f.state, settled = f, updatedAtMs = now)
            repo.putItem(settled)
            repo.closeOutboxOf(serverId, f.requestId, RESULT_SETTLED, now)
            events += ItemEvent.Settled(settled)
        }
        Outcome(f.id, events)
    }

    fun onDigest(serverId: Long, f: DigestFrame): Outcome = repo.transaction {
        if (repo.hasDelivery(serverId, f.id)) return@transaction Outcome(f.id, emptyList())
        repo.addDelivery(serverId, f.id, clock())
        Outcome(f.id, listOf(ItemEvent.Digest(serverId, f.count, f.oldestCreatedAtMs, f.ids)))
    }

    /** The result of an answer or a reply. It has no delivery id: nothing to acknowledge. */
    fun onAnswerResult(serverId: Long, f: AnswerResultFrame): Outcome = repo.transaction {
        val entry = repo.outbox(serverId, f.clientMsgId)
        if (entry == null || !entry.isPending) return@transaction Outcome.NONE
        val now = clock()
        repo.closeOutbox(serverId, f.clientMsgId, f.status.wire, now)
        val item = repo.item(serverId, f.requestId)
        if (item != null) {
            val next = afterResult(item, entry, f)
            if (next != item) repo.putItem(next.copy(updatedAtMs = now))
        }
        Outcome(null, listOf(ItemEvent.AnswerResult(serverId, f, entry.copy(result = f.status.wire, closedAtMs = now))))
    }

    /** The item after a result. The `state` of the result is the state that the server holds. */
    private fun afterResult(item: Item, entry: OutboxEntry, f: AnswerResultFrame): Item {
        val byStatus: ItemState? = when (f.status) {
            AnswerStatus.ACCEPTED -> if (entry.type == OutboxType.REPLY) ItemState.REPLIED else ItemState.ANSWERED
            AnswerStatus.ALREADY_ANSWERED -> ItemState.ANSWERED
            AnswerStatus.ABANDONED -> ItemState.ABANDONED
            AnswerStatus.SUPERSEDED -> ItemState.SUPERSEDED
            AnswerStatus.EXPIRED -> ItemState.EXPIRED
            AnswerStatus.CANCELLED -> ItemState.CANCELLED
            AnswerStatus.CLOSED, AnswerStatus.EFFECT_MISMATCH, AnswerStatus.DEVICE_REVOKED,
            AnswerStatus.CUSTOM_NOT_ALLOWED, AnswerStatus.ASK_BACK_NOT_ALLOWED -> null
        }
        val state = f.state ?: byStatus
        var next = item
        // A final item stays final: only a newer `request` frame revives it.
        if (state != null && item.isOpen) next = next.copy(state = state)
        // The server says that the effect differs from what the phone showed: never approve it.
        if (f.status == AnswerStatus.EFFECT_MISMATCH) next = next.copy(validity = Validity.EFFECT_MISMATCH)
        return next
    }

    // ------------------------------------------------------------ session

    /**
     * Stores what the `welcome` says (the preferences and the purge age) and gives the frames
     * to send: the `sync` of the open items, then the outbox. A server without items
     * (`body_purge_after_ms` 0) gets nothing: it answers `not_enabled` to these frames.
     */
    fun onWelcome(serverId: Long, w: WelcomeFrame): SessionStart = repo.transaction {
        repo.putPrefs(serverId, StoredPrefs(w.prefs, w.bodyPurgeAfterMs), clock())
        if (!w.itemsEnabled) return@transaction SessionStart(null, emptyList())
        SessionStart(syncFrame(serverId), repo.pendingOutbox(serverId))
    }

    /**
     * The items that the phone holds open, with the last turn that it knows. When more than 500
     * are open, the oldest 500 go: the server also sends each `settled` frame that the phone missed.
     */
    fun syncFrame(serverId: Long): SyncFrame = SyncFrame(
        repo.openItems(serverId).take(MAX_SYNC_ENTRIES).map {
            val lastTurn = repo.turns(serverId, it.requestId).maxOfOrNull { t -> t.seq } ?: 0L
            OpenRef(it.requestId, maxOf(it.request.seq, lastTurn))
        },
    )

    fun pendingOutbox(serverId: Long): List<OutboxEntry> = repo.pendingOutbox(serverId)

    /** Records that the frame went out on a session. */
    fun markSent(entry: OutboxEntry) = repo.markSent(entry.serverId, entry.clientMsgId, clock())

    // ------------------------------------------------------------ what the owner does

    /** Approves an approval. The frame carries the effect that the phone computed from the `op` it received. */
    fun approve(serverId: Long, requestId: String): Submit {
        val item = repo.item(serverId, requestId) ?: return refused(Refusal.NOT_FOUND)
        val approval = item.request.approval
        if (item.kind != RequestKind.APPROVAL || approval == null) return refused(Refusal.NOT_AN_APPROVAL)
        if (!item.isOpen) return refused(Refusal.NOT_OPEN)
        if (!item.validity.isValid) return refused(Refusal.INVALID_REQUEST)
        if (hasPendingAnswer(serverId, requestId)) return refused(Refusal.ALREADY_QUEUED)
        // Compute again at the moment of the answer: the hash that goes out is the hash of this `op`.
        val verdict = ShownEffect.verify(approval, hasher)
        if (verdict !is ShownEffect.Verdict.Valid) return refused(Refusal.INVALID_REQUEST)
        return queue(serverId, requestId, OutboxType.ANSWER) {
            AnswerFrame(requestId, it, Action.ACCEPT, shownEffect = verdict.effect)
        }
    }

    /**
     * Declines an item. A text only goes with a decline of an approval whose `allow_custom` is
     * true, and an `intent` needs the text. A decline of a question carries no text.
     */
    fun decline(serverId: Long, requestId: String, text: String? = null, intent: Intent? = null): Submit {
        val item = repo.item(serverId, requestId) ?: return refused(Refusal.NOT_FOUND)
        if (!item.isOpen) return refused(Refusal.NOT_OPEN)
        if (hasPendingAnswer(serverId, requestId)) return refused(Refusal.ALREADY_QUEUED)
        if (text != null) {
            if (item.kind != RequestKind.APPROVAL) return refused(Refusal.BAD_INPUT)
            if (!item.request.allowCustom) return refused(Refusal.CUSTOM_NOT_ALLOWED)
        }
        return queue(serverId, requestId, OutboxType.ANSWER) {
            AnswerFrame(requestId, it, Action.DECLINE, text = text, intent = intent)
        }
    }

    /** Asks the agent a question. The item stays open. One message waits in the outbox at a time. */
    fun reply(serverId: Long, requestId: String, text: String): Submit {
        val item = repo.item(serverId, requestId) ?: return refused(Refusal.NOT_FOUND)
        if (!item.isOpen) return refused(Refusal.NOT_OPEN)
        if (!item.request.allowAskBack) return refused(Refusal.ASK_BACK_NOT_ALLOWED)
        // The owner has one message at a time (DIAL 3): the next one waits for the result of this one.
        if (repo.pendingOutbox(serverId).any { it.requestId == requestId && it.type == OutboxType.REPLY }) return refused(Refusal.ALREADY_QUEUED)
        return queue(serverId, requestId, OutboxType.REPLY) { ReplyFrame(requestId, it, text) }
    }

    private fun hasPendingAnswer(serverId: Long, requestId: String): Boolean =
        repo.pendingOutbox(serverId).any { it.requestId == requestId && it.type == OutboxType.ANSWER }

    private fun refused(reason: Refusal) = Submit.Refused(reason)

    private fun queue(serverId: Long, requestId: String, type: OutboxType, make: (String) -> ClientFrame): Submit {
        val id = newMsgId()
        val frame = make(id)
        val text = try {
            String(ClientFrames.encode(frame), Charsets.UTF_8)
        } catch (e: FrameException) {
            return refused(Refusal.BAD_INPUT)
        }
        val entry = OutboxEntry(serverId, id, requestId, type, text, clock(), 0, null, null, null)
        repo.putOutbox(entry)
        return Submit.Queued(entry)
    }

    companion object {
        /** The `result` of an outbox entry that a `settled` frame closed. */
        const val RESULT_SETTLED = "settled"
    }
}
