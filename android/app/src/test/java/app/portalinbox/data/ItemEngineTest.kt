package app.portalinbox.data

import app.portalinbox.core.EffectResult
import app.portalinbox.core.ViewResult
import app.portalinbox.proto.Action
import app.portalinbox.proto.AnswerFrame
import app.portalinbox.proto.ClientFrames
import app.portalinbox.proto.Intent
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.OpenRef
import app.portalinbox.proto.ReplyFrame
import app.portalinbox.proto.SyncFrame
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.ReferenceEffectHasher
import app.portalinbox.testsupport.ScriptedEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class ItemEngineTest {
    private val server = 1L
    private var now = 1_800_000_000_000L
    private val repo = MemoryItemRepository()
    private val hasher = ReferenceEffectHasher()
    private var msg = 0
    private val engine = ItemEngine(repo, hasher, { now }, { "m-%04d".format(++msg) })

    private val rid = Make.hex(0xb)
    private fun item(id: String = rid): Item = repo.item(server, id)!!

    private fun open(id: String = rid, delivery: Int = 1, seq: Long = 0): Item {
        engine.onRequest(server, Make.approval(id, Make.hex(0xd00 + delivery), seq))
        return item(id)
    }

    // ------------------------------------------------------------ state table

    @Test
    fun aRequestMakesAnOpenItemAndGivesTheReceipt() {
        val frame = Make.approval(rid, Make.hex(1))
        val out = engine.onRequest(server, frame)
        assertEquals(Make.hex(1), out.ack)
        val i = item()
        assertEquals(ItemState.PENDING, i.state)
        assertTrue(i.isOpen)
        assertEquals(Validity.VALID, i.validity)
        assertEquals(frame.approval!!.effect, i.computedEffect)
        assertEquals("INSERT INTO analytics.events SELECT 1", i.commandText)
        assertTrue(i.canApprove)
        assertNull(i.settled)
        assertEquals(listOf<ItemEvent>(ItemEvent.NewRequest(i)), out.events)
        assertFalse(i.notified)
    }

    @Test
    fun aQuestionIsStoredWithoutAHashCheck() {
        val out = engine.onRequest(server, Make.question(rid, Make.hex(1)))
        val i = item()
        assertEquals(ItemState.PENDING, i.state)
        assertEquals(Validity.VALID, i.validity)
        assertNull(i.computedEffect)
        assertNull(i.view)
        assertFalse(i.canApprove)
        assertEquals("DELETE FROM staging.events WHERE ts < '2026-11-01';", i.commandText)
        assertEquals(1, out.events.size)
        assertEquals(0, hasher.effectCalls)
    }

    @Test
    fun theSameDeliveryTwiceChangesNothingAndIsAcknowledgedAgain() {
        val frame = Make.approval(rid, Make.hex(1))
        engine.onRequest(server, frame)
        val before = item()
        now += 5_000
        val again = engine.onRequest(server, frame)
        assertEquals(Make.hex(1), again.ack)
        assertTrue(again.events.isEmpty())
        assertEquals(before, item())
        // The same holds for each frame of the queue.
        val settled = Make.settled(rid, Make.hex(2))
        assertEquals(1, engine.onSettled(server, settled).events.size)
        val s1 = item()
        now += 5_000
        val s2 = engine.onSettled(server, settled)
        assertEquals(Make.hex(2), s2.ack)
        assertTrue(s2.events.isEmpty())
        assertEquals(s1, item())
        val m = Make.message(rid, Make.hex(3), 1, user = false)
        assertEquals(1, engine.onMessage(server, m).events.size)
        assertTrue(engine.onMessage(server, m).events.isEmpty())
        assertEquals(1, repo.turns(server, rid).size)
        val d = Make.digest(Make.hex(4))
        // The event carries the delivery id: a new digest is a new thing for the one notification.
        assertEquals(Make.hex(4), (engine.onDigest(server, d).events.single() as ItemEvent.Digest).deliveryId)
        assertEquals(Make.hex(4), engine.onDigest(server, d).ack)
        assertTrue(engine.onDigest(server, d).events.isEmpty())
    }

    @Test
    fun anOlderOrEqualSeqDoesNotChangeTheItemAndANewerOneReplacesIt() {
        open(seq = 2)
        val received = item().receivedAtMs
        // Same seq, new delivery: the same state again.
        assertTrue(engine.onRequest(server, Make.approval(rid, Make.hex(20), 2, sql = "SELECT 2")).events.isEmpty())
        // Older seq.
        assertTrue(engine.onRequest(server, Make.approval(rid, Make.hex(21), 1, sql = "SELECT 3")).events.isEmpty())
        assertEquals("INSERT INTO analytics.events SELECT 1", item().commandText)
        now += 1_000
        val out = engine.onRequest(server, Make.approval(rid, Make.hex(22), 3, sql = "SELECT 4"))
        assertEquals(1, out.events.size)
        assertTrue(out.events[0] is ItemEvent.Updated)
        assertEquals("SELECT 4", item().commandText)
        assertEquals(3L, item().request.seq)
        assertEquals(received, item().receivedAtMs)
        assertEquals(ItemState.PENDING, item().state)
    }

    @Test
    fun settledMakesTheItemFinalWithItsDetails() {
        open()
        val out = engine.onSettled(server, Make.settled(rid, Make.hex(2)))
        val i = item()
        assertEquals(ItemState.ANSWERED, i.state)
        assertFalse(i.isOpen)
        assertEquals("answered", i.settled!!.reason)
        assertEquals(app.portalinbox.proto.Decision.APPROVE, i.settled!!.decision)
        assertEquals(listOf<ItemEvent>(ItemEvent.Settled(i)), out.events)
        assertFalse(i.canApprove)
        // Each final state that the server sends.
        val finals = listOf("settled-abandoned.json" to ItemState.ABANDONED, "settled-superseded.json" to ItemState.SUPERSEDED)
        for ((n, pair) in finals.withIndex()) {
            val id = Make.hex(0x100 + n)
            engine.onRequest(server, Make.approval(id, Make.hex(0x300 + n)))
            engine.onSettled(server, Make.settled(id, Make.hex(0x400 + n), pair.first))
            assertEquals(pair.second, item(id).state)
        }
    }

    @Test
    fun aSettledFrameBeforeItsRequestIsKeptAndTheRequestArrivesFinal() {
        val early = engine.onSettled(server, Make.settled(rid, Make.hex(2)))
        assertEquals(Make.hex(2), early.ack)
        assertTrue(early.events.isEmpty())
        assertNull(repo.item(server, rid))
        now += 1_000
        val out = engine.onRequest(server, Make.approval(rid, Make.hex(1)))
        assertEquals(Make.hex(1), out.ack)
        // No new open item: nothing to notify, and nothing to show in "To answer".
        assertTrue(out.events.isEmpty())
        assertEquals(ItemState.ANSWERED, item().state)
        assertNotNull(item().settled)
        assertTrue(repo.openItems(server).isEmpty())
        // The kept frame is used once.
        assertNull(repo.takeEarlySettlement(server, rid))
    }

    @Test
    fun theTurnsDecideWhoseMoveItIs() {
        open()
        val user = engine.onMessage(server, Make.message(rid, Make.hex(10), 1, user = true))
        assertTrue(user.events[0] is ItemEvent.UserMessage)
        assertEquals(ItemState.REPLIED, item().state)
        assertTrue(item().isOpen)
        val agent = engine.onMessage(server, Make.message(rid, Make.hex(11), 2, user = false))
        assertTrue(agent.events[0] is ItemEvent.AgentMessage)
        assertEquals(ItemState.PENDING, item().state)
        // The same turn under another delivery id is not a new turn.
        assertTrue(engine.onMessage(server, Make.message(rid, Make.hex(12), 2, user = false)).events.isEmpty())
        assertEquals(2, repo.turns(server, rid).size)
        // After the item is final, a late turn is stored and the state stays.
        engine.onSettled(server, Make.settled(rid, Make.hex(13)))
        engine.onMessage(server, Make.message(rid, Make.hex(14), 3, user = true))
        assertEquals(ItemState.ANSWERED, item().state)
        assertEquals(3, repo.turns(server, rid).size)
    }

    @Test
    fun aTurnBeforeTheRequestIsStored() {
        val out = engine.onMessage(server, Make.message(rid, Make.hex(10), 1, user = false))
        assertEquals(Make.hex(10), out.ack)
        assertEquals(1, repo.turns(server, rid).size)
        engine.onRequest(server, Make.approval(rid, Make.hex(1), seq = 1))
        assertEquals(ItemState.PENDING, item().state)
    }

    @Test
    fun anAbandonedItemIsRevivedByANewerRequestOnly() {
        open(seq = 0)
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        assertEquals(ItemState.ABANDONED, item().state)
        // A retransmission of the same request does not revive it.
        assertTrue(engine.onRequest(server, Make.approval(rid, Make.hex(30), 0)).events.isEmpty())
        assertEquals(ItemState.ABANDONED, item().state)
        val out = engine.onRequest(server, Make.approval(rid, Make.hex(31), 1))
        assertTrue(out.events[0] is ItemEvent.Revived)
        assertEquals(ItemState.PENDING, item().state)
        assertNull(item().settled)
        assertEquals(listOf(rid), repo.openItems(server).map { it.requestId })
        // The old `settled` frame sent again (the same delivery) does not abandon the item again.
        assertTrue(engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json")).events.isEmpty())
        assertEquals(ItemState.PENDING, item().state)
    }

    @Test
    fun aFinalItemOtherThanAbandonedStaysFinal() {
        open()
        engine.onSettled(server, Make.settled(rid, Make.hex(2)))
        assertTrue(engine.onRequest(server, Make.approval(rid, Make.hex(3), 5)).events.isEmpty())
        assertEquals(ItemState.ANSWERED, item().state)
    }

    // ------------------------------------------------------------ invalid requests

    @Test
    fun aRequestWithAWrongEffectIsStoredAsInvalidAndNeverApproved() {
        val wrong = "sha256:" + "0".repeat(64)
        val out = engine.onRequest(server, Make.approval(rid, Make.hex(1), effect = wrong))
        assertEquals(Make.hex(1), out.ack)
        val i = item()
        assertEquals(Validity.EFFECT_MISMATCH, i.validity)
        assertEquals(ItemState.PENDING, i.state)
        assertNull(i.computedEffect)
        assertNull(i.view)
        assertNull(i.commandText)
        assertFalse(i.canApprove)
        assertEquals(Submit.Refused(Refusal.INVALID_REQUEST), engine.approve(server, rid))
        // The owner can still decline it.
        assertTrue(engine.decline(server, rid) is Submit.Queued)
    }

    @Test
    fun anOpThatTheHasherRefusesAndAViewThatNamesNothingAreInvalidToo() {
        val scripted = ScriptedEffectHasher(
            { _, _, _, _ -> EffectResult.Failure("invalid_op", "x") },
            { _, _ -> ViewResult.Failure("invalid_view", "y") },
        )
        val e = ItemEngine(repo, scripted, { now })
        e.onRequest(server, Make.approval(Make.hex(5), Make.hex(1)))
        assertEquals(Validity.INVALID_OP, item(Make.hex(5)).validity)
        // The hash is right, the view is wrong.
        scripted.effectResult = { s, a, r, op -> hasher.effect(s, a, r, op) }
        e.onRequest(server, Make.approval(Make.hex(6), Make.hex(2)))
        assertEquals(Validity.INVALID_VIEW, item(Make.hex(6)).validity)
        // A pointer that the real code finds missing.
        val missing = Make.approval(Make.hex(8), Make.hex(4)) { o -> o.getJSONObject("approval").getJSONObject("view").put("body", "/missing") }
        engine.onRequest(server, missing)
        assertEquals(Validity.INVALID_VIEW, item(Make.hex(8)).validity)
        assertFalse(item(Make.hex(8)).canApprove)
    }

    @Test
    fun aFailureOfTheHasherStopsTheFrameBeforeAnythingIsStored() {
        val boom = ItemEngine(repo, ScriptedEffectHasher({ _, _, _, _ -> throw IllegalStateException("no library") }, { _, _ -> throw IllegalStateException() }), { now })
        val frame = Make.approval(rid, Make.hex(1))
        assertThrows(IllegalStateException::class.java) { boom.onRequest(server, frame) }
        // Nothing stays: not the item, not the delivery id. The server sends the row again.
        assertNull(repo.item(server, rid))
        assertFalse(repo.hasDelivery(server, Make.hex(1)))
        assertEquals(Make.hex(1), engine.onRequest(server, frame).ack)
        assertEquals(ItemState.PENDING, item().state)
    }

    // ------------------------------------------------------------ outbox

    @Test
    fun approveQueuesAnAnswerWithTheEffectThatThePhoneComputed() {
        open()
        now += 1
        val sub = engine.approve(server, rid) as Submit.Queued
        val e = sub.entry
        assertEquals("m-0001", e.clientMsgId)
        assertEquals(OutboxType.ANSWER, e.type)
        assertTrue(e.isPending)
        val frame = e.toFrame() as AnswerFrame
        assertEquals(Action.ACCEPT, frame.action)
        assertEquals(item().computedEffect, frame.shownEffect)
        assertEquals(item().request.approval!!.effect, frame.shownEffect)
        assertNull(frame.text)
        assertEquals(listOf(e), repo.pendingOutbox(server))
        // The frame in the outbox is the frame that goes on the wire.
        assertEquals(e.frame, String(ClientFrames.encode(frame)))
    }

    @Test
    fun oneAnswerWaitsForItsResultBeforeAnotherIsQueued() {
        open()
        assertTrue(engine.approve(server, rid) is Submit.Queued)
        assertEquals(Submit.Refused(Refusal.ALREADY_QUEUED), engine.approve(server, rid))
        assertEquals(Submit.Refused(Refusal.ALREADY_QUEUED), engine.decline(server, rid))
        // A reply is another kind of frame: it is not an answer.
        assertTrue(engine.reply(server, rid, "Why?") is Submit.Queued)
        assertEquals(2, repo.pendingOutbox(server).size)
    }

    @Test
    fun theOutboxIsSentAgainAfterEachReconnectionUntilTheResultArrives() {
        open()
        val e = (engine.approve(server, rid) as Submit.Queued).entry
        // First session.
        var start = engine.onWelcome(server, Make.welcome())
        assertEquals(listOf(e.clientMsgId), start.outbox.map { it.clientMsgId })
        engine.markSent(start.outbox[0])
        assertEquals(1, repo.outbox(server, e.clientMsgId)!!.attempts)
        // The link drops before the result. Second session: the same frame again.
        now += 60_000
        start = engine.onWelcome(server, Make.welcome())
        assertEquals(listOf(e.clientMsgId), start.outbox.map { it.clientMsgId })
        assertEquals(e.frame, start.outbox[0].frame)
        engine.markSent(start.outbox[0])
        assertEquals(2, repo.outbox(server, e.clientMsgId)!!.attempts)
        // The result closes the entry and the item follows the state of the server.
        val out = engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted"))
        assertNull(out.ack)
        val ev = out.events.single() as ItemEvent.AnswerResult
        assertEquals("accepted", ev.entry.result)
        assertTrue(repo.pendingOutbox(server).isEmpty())
        assertEquals("accepted", repo.outbox(server, e.clientMsgId)!!.result)
        assertEquals(ItemState.ANSWERED, item().state)
        // Third session: nothing to send.
        assertTrue(engine.onWelcome(server, Make.welcome()).outbox.isEmpty())
        // A result for the same message again is ignored, as is a result for an unknown one.
        assertTrue(engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted")).events.isEmpty())
        assertTrue(engine.onAnswerResult(server, Make.result(rid, "m-9999", "accepted")).events.isEmpty())
    }

    @Test
    fun aSettledFrameClosesTheOutboxOfTheItem() {
        open()
        val e = (engine.approve(server, rid) as Submit.Queued).entry
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        val closed = repo.outbox(server, e.clientMsgId)!!
        assertFalse(closed.isPending)
        assertEquals(ItemEngine.RESULT_SETTLED, closed.result)
        assertTrue(engine.onWelcome(server, Make.welcome()).outbox.isEmpty())
        // The result that comes after the close is ignored: the settled state stays.
        assertTrue(engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "abandoned")).events.isEmpty())
        // The same for the early `settled` of an item that arrives later.
        val other = Make.hex(0xe)
        engine.onSettled(server, Make.settled(other, Make.hex(5)))
        engine.onRequest(server, Make.approval(other, Make.hex(6)))
        assertEquals(ItemState.ANSWERED, item(other).state)
    }

    @Test
    fun resultsMoveTheItemAsTheServerSaysAndNeverReopenAFinalItem() {
        var n = 0
        fun run(status: String, state: String?): Item {
            n++
            val id = Make.hex(0x500 + n)
            engine.onRequest(server, Make.approval(id, Make.hex(0x600 + n)))
            val e = (engine.decline(server, id) as Submit.Queued).entry
            engine.onAnswerResult(server, Make.result(id, e.clientMsgId, status, state))
            assertFalse(repo.outbox(server, e.clientMsgId)!!.isPending)
            assertEquals(status, repo.outbox(server, e.clientMsgId)!!.result)
            return item(id)
        }
        assertEquals(ItemState.ANSWERED, run("accepted", null).state)
        assertEquals(ItemState.ANSWERED, run("already_answered", "answered").state)
        assertEquals(ItemState.ABANDONED, run("abandoned", null).state)
        assertEquals(ItemState.EXPIRED, run("expired", "expired").state)
        assertEquals(ItemState.CANCELLED, run("cancelled", null).state)
        assertEquals(ItemState.SUPERSEDED, run("superseded", "superseded").state)
        // These statuses leave the state alone: the `settled` frame or a later frame tells it.
        for ((status, state) in listOf("closed" to null, "device_revoked" to null, "custom_not_allowed" to null, "ask_back_not_allowed" to null)) {
            assertEquals(status, ItemState.PENDING, run(status, state).state)
        }
        // The server says that the effect differs: the item becomes invalid.
        val i = run("effect_mismatch", null)
        assertEquals(Validity.EFFECT_MISMATCH, i.validity)
        assertEquals(ItemState.PENDING, i.state)
        assertFalse(i.canApprove)
    }

    @Test
    fun aReplyKeepsTheItemOpenAsReplied() {
        open()
        val e = (engine.reply(server, rid, "Why this database?") as Submit.Queued).entry
        val f = e.toFrame() as ReplyFrame
        assertEquals("Why this database?", f.text)
        assertEquals(OutboxType.REPLY, e.type)
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted", "replied"))
        assertEquals(ItemState.REPLIED, item().state)
        assertTrue(item().isOpen)
        // Without a `state` in the result, an accepted reply is `replied` too.
        val e2 = (engine.reply(server, rid, "And the host?") as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e2.clientMsgId, "accepted"))
        assertEquals(ItemState.REPLIED, item().state)
    }

    // ------------------------------------------------------------ what the owner may do

    @Test
    fun theFlagsOfTheItemDecideWhatTheOwnerMaySend() {
        engine.onRequest(server, Make.approval(rid, Make.hex(1), allowCustom = false, allowAskBack = false))
        assertEquals(Submit.Refused(Refusal.CUSTOM_NOT_ALLOWED), engine.decline(server, rid, "no", Intent.COMMENT))
        assertEquals(Submit.Refused(Refusal.ASK_BACK_NOT_ALLOWED), engine.reply(server, rid, "why"))
        assertTrue(engine.decline(server, rid) is Submit.Queued)

        val id2 = Make.hex(0xc)
        engine.onRequest(server, Make.approval(id2, Make.hex(2)))
        val e = (engine.decline(server, id2, "Use staging.", Intent.CHANGE_REQUEST) as Submit.Queued).entry
        val f = e.toFrame() as AnswerFrame
        assertEquals(Action.DECLINE, f.action)
        assertEquals("Use staging.", f.text)
        assertEquals(Intent.CHANGE_REQUEST, f.intent)
        assertNull(f.shownEffect)
    }

    @Test
    fun aBadTextOrACombinationThatBreaksTheFrameIsRefusedBeforeItIsQueued() {
        open()
        assertEquals(Submit.Refused(Refusal.BAD_INPUT), engine.decline(server, rid, "", Intent.COMMENT))
        assertEquals(Submit.Refused(Refusal.BAD_INPUT), engine.decline(server, rid, "x".repeat(8001)))
        assertEquals(Submit.Refused(Refusal.BAD_INPUT), engine.decline(server, rid, "a\u0000b"))
        assertEquals(Submit.Refused(Refusal.BAD_INPUT), engine.reply(server, rid, ""))
        assertEquals(Submit.Refused(Refusal.BAD_INPUT), engine.decline(server, rid, null, Intent.COMMENT))
        assertTrue(repo.pendingOutbox(server).isEmpty())
        assertEquals(Submit.Refused(Refusal.NOT_FOUND), engine.approve(server, Make.hex(0x99)))
    }

    @Test
    fun aQuestionIsNotApprovedAndItsDeclineCarriesNoText() {
        engine.onRequest(server, Make.question(rid, Make.hex(1)))
        assertEquals(Submit.Refused(Refusal.NOT_AN_APPROVAL), engine.approve(server, rid))
        assertEquals(Submit.Refused(Refusal.BAD_INPUT), engine.decline(server, rid, "no thanks"))
        assertTrue(engine.decline(server, rid) is Submit.Queued)
    }

    @Test
    fun aClosedItemTakesNoAnswerAndNoReply() {
        open()
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        assertEquals(Submit.Refused(Refusal.NOT_OPEN), engine.approve(server, rid))
        assertEquals(Submit.Refused(Refusal.NOT_OPEN), engine.decline(server, rid))
        assertEquals(Submit.Refused(Refusal.NOT_OPEN), engine.reply(server, rid, "hello"))
    }

    // ------------------------------------------------------------ session

    @Test
    fun theWelcomeStoresThePreferencesAndTheSyncListsTheOpenItems() {
        val a = Make.hex(0xa1)
        val b = Make.hex(0xa2)
        val c = Make.hex(0xa3)
        engine.onRequest(server, Make.approval(a, Make.hex(1), seq = 0, createdAtMs = 3_000))
        engine.onRequest(server, Make.approval(b, Make.hex(2), seq = 2, createdAtMs = 1_000))
        engine.onRequest(server, Make.approval(c, Make.hex(3), createdAtMs = 2_000))
        engine.onMessage(server, Make.message(a, Make.hex(4), 5, user = false))
        engine.onSettled(server, Make.settled(c, Make.hex(5)))
        val start = engine.onWelcome(server, Make.welcome())
        assertEquals(
            // The oldest first. `a` knows turn 5, `b` has the seq of its request, `c` is final and absent.
            SyncFrame(listOf(OpenRef(b, 2), OpenRef(a, 5))),
            start.sync,
        )
        assertTrue(start.outbox.isEmpty())
        val stored = repo.prefs(server)!!
        assertEquals(2_592_000_000L, stored.bodyPurgeAfterMs)
        assertEquals("22:00", stored.prefs.quiet!!.from)
        assertEquals("daily", stored.prefs.digest!!.mode.wire)
        // The sync frame is a valid frame of the protocol.
        assertNotNull(ClientFrames.encode(start.sync!!))
    }

    @Test
    fun aSyncWithNoOpenItemIsStillSent() {
        val start = engine.onWelcome(server, Make.welcome())
        assertEquals(SyncFrame(emptyList()), start.sync)
    }

    @Test
    fun aSyncHoldsAtMost500Entries() {
        for (n in 1..501) engine.onRequest(server, Make.approval(Make.hex(0x1000 + n), Make.hex(0x2000 + n), createdAtMs = n.toLong()))
        val sync = engine.syncFrame(server)
        assertEquals(500, sync.open.size)
        assertEquals(Make.hex(0x1001), sync.open.first().requestId)
        assertNotNull(ClientFrames.encode(sync))
    }

    @Test
    fun aServerWithoutItemsGetsNoSyncAndNoOutboxButItsPreferencesAreStored() {
        open()
        val e = (engine.approve(server, rid) as Submit.Queued).entry
        val start = engine.onWelcome(server, Make.welcome("welcome-empty-prefs.json"))
        assertNull(start.sync)
        assertTrue(start.outbox.isEmpty())
        assertEquals(0L, repo.prefs(server)!!.bodyPurgeAfterMs)
        // The entry waits: a later server that handles items gets it.
        assertTrue(repo.outbox(server, e.clientMsgId)!!.isPending)
        assertEquals(1, engine.onWelcome(server, Make.welcome()).outbox.size)
    }

    @Test
    fun theItemsOfAnotherServerAreSeparate() {
        engine.onRequest(1, Make.approval(rid, Make.hex(1)))
        engine.onRequest(2, Make.approval(rid, Make.hex(1)))
        engine.onSettled(2, Make.settled(rid, Make.hex(2)))
        assertEquals(ItemState.PENDING, repo.item(1, rid)!!.state)
        assertEquals(ItemState.ANSWERED, repo.item(2, rid)!!.state)
        assertEquals(1, engine.syncFrame(1).open.size)
        assertEquals(0, engine.syncFrame(2).open.size)
    }

    @Test
    fun onFrameSendsEachFrameToItsHandlerAndIgnoresTheOthers() {
        val out = engine.onFrame(server, Make.approval(rid, Make.hex(1)))
        assertEquals(Make.hex(1), out.ack)
        assertEquals(Outcome.NONE, engine.onFrame(server, Make.welcome()))
        assertEquals(Outcome.NONE, engine.onFrame(server, app.portalinbox.proto.PongFrame(1)))
        assertEquals(Make.hex(5), engine.onFrame(server, Make.digest(Make.hex(5))).ack)
    }
}
