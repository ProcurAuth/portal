package app.portalinbox.data

import app.portalinbox.proto.Action
import app.portalinbox.proto.Intent
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class OutboxAnswerTest {
    private val server = 1L
    private var now = 1_800_000_000_000L
    private val repo = MemoryItemRepository()
    private var msg = 0
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now }, { "m-%04d".format(++msg) })
    private val rid = Make.hex(0xb)

    private fun open() {
        engine.onRequest(server, Make.approval(rid, Make.hex(1)))
    }

    private fun progress() = OutboxAnswer.progress(repo.outboxOf(server, rid))

    @Test
    fun noAnswerGivesNone() {
        open()
        assertEquals(AnswerProgress.None, progress())
        assertFalse(progress().pending)
        assertNull(OutboxAnswer.pendingLine(progress()))
    }

    @Test
    fun anApproveIsQueuedUntilASessionSendsIt() {
        open()
        val r = engine.approve(server, rid) as Submit.Queued
        assertEquals(AnswerProgress.Queued(Action.ACCEPT), progress())
        assertTrue(progress().pending)
        assertEquals("Your answer is saved. It goes out when the phone is online.", OutboxAnswer.pendingLine(progress()))
        engine.markSent(r.entry)
        assertEquals(AnswerProgress.Sent(Action.ACCEPT), progress())
        assertEquals("Your answer is sent. Waiting for the server.", OutboxAnswer.pendingLine(progress()))
    }

    @Test
    fun theAnswerFrameCarriesTheEffectTheScreenShowed() {
        open()
        val r = engine.approve(server, rid) as Submit.Queued
        val frame = r.entry.toFrame() as app.portalinbox.proto.AnswerFrame
        assertEquals(Action.ACCEPT, frame.action)
        assertEquals(repo.item(server, rid)!!.computedEffect, frame.shownEffect)
        assertNull(frame.text)
    }

    @Test
    fun anAcceptedResultClosesTheEntryAndTheItemIsAnswered() {
        open()
        val r = engine.approve(server, rid) as Submit.Queued
        engine.markSent(r.entry)
        engine.onAnswerResult(server, Make.result(rid, r.entry.clientMsgId, "accepted"))
        val p = progress() as AnswerProgress.Done
        assertEquals(Action.ACCEPT, p.action)
        assertEquals("You approved this request.", OutboxAnswer.resultLine(p))
        assertFalse(p.pending)
        assertEquals(app.portalinbox.proto.ItemState.ANSWERED, repo.item(server, rid)!!.state)
    }

    @Test
    fun aDeclineWithACommentKeepsTextAndIntentInTheOutbox() {
        open()
        val r = engine.decline(server, rid, "use staging_b", Intent.CHANGE_REQUEST) as Submit.Queued
        assertEquals(AnswerProgress.Queued(Action.DECLINE), progress())
        assertEquals("use staging_b", OutboxAnswer.declineText(r.entry))
        assertNull(OutboxAnswer.replyText(r.entry))
        engine.onAnswerResult(server, Make.result(rid, r.entry.clientMsgId, "accepted"))
        assertEquals("You declined this request.", OutboxAnswer.resultLine(progress() as AnswerProgress.Done))
    }

    @Test
    fun aSecondAnswerWhileOneWaitsIsRefusedAndTheMessageSaysSo() {
        open()
        engine.approve(server, rid)
        val again = engine.decline(server, rid)
        assertEquals(Submit.Refused(Refusal.ALREADY_QUEUED), again)
        assertEquals("An answer is already on its way.", OutboxAnswer.submitLine(again))
    }

    @Test
    fun anAnswerOfAnInvalidRequestIsRefusedWithAClearMessage() {
        engine.onRequest(server, Make.approval(rid, Make.hex(1), effect = "sha256:" + "0".repeat(64)))
        val r = engine.approve(server, rid)
        assertEquals(Submit.Refused(Refusal.INVALID_REQUEST), r)
        assertEquals("Invalid request. The phone does not approve it.", OutboxAnswer.submitLine(r))
    }

    @Test
    fun aQueuedAnswerHasNoRefusalLine() {
        open()
        assertNull(OutboxAnswer.submitLine(engine.approve(server, rid)))
    }

    @Test
    fun aSettledFrameClosesTheEntryAndTheLateAnswerSaysNothing() {
        open()
        engine.approve(server, rid)
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        val p = progress() as AnswerProgress.Done
        assertEquals(ItemEngine.RESULT_SETTLED, p.result)
        assertNull(OutboxAnswer.resultLine(p))
        assertFalse(p.pending)
    }

    @Test
    fun tooLateResultsUseTheWordsTooLate() {
        open()
        val r = engine.approve(server, rid) as Submit.Queued
        engine.onAnswerResult(server, Make.result(rid, r.entry.clientMsgId, "abandoned", "abandoned"))
        val p = progress() as AnswerProgress.Done
        assertEquals("Too late: the agent no longer answers.", OutboxAnswer.resultLine(p))
        assertFalse(OutboxAnswer.canTryAgain(p))
    }

    @Test
    fun anEffectMismatchMakesTheRequestInvalidAndAsksToReadAgain() {
        open()
        val r = engine.approve(server, rid) as Submit.Queued
        engine.onAnswerResult(server, Make.result(rid, r.entry.clientMsgId, "effect_mismatch"))
        val p = progress() as AnswerProgress.Done
        assertEquals("The command changed. Open the request again and read it.", OutboxAnswer.resultLine(p))
        assertEquals(Validity.EFFECT_MISMATCH, repo.item(server, rid)!!.validity)
    }

    @Test
    fun aResultThatLeavesTheItemOpenLetsTheOwnerTryAgain() {
        open()
        val r = engine.decline(server, rid, "text", Intent.COMMENT) as Submit.Queued
        engine.onAnswerResult(server, Make.result(rid, r.entry.clientMsgId, "custom_not_allowed"))
        val p = progress() as AnswerProgress.Done
        assertTrue(OutboxAnswer.canTryAgain(p))
        assertTrue(repo.item(server, rid)!!.isOpen)
        // The next answer is accepted by the engine: the first entry is closed.
        assertTrue(engine.decline(server, rid) is Submit.Queued)
    }

    @Test
    fun theLastAnswerWinsInTheProgress() {
        open()
        val a = engine.decline(server, rid, "text", Intent.COMMENT) as Submit.Queued
        engine.onAnswerResult(server, Make.result(rid, a.entry.clientMsgId, "custom_not_allowed"))
        now += 1000
        engine.approve(server, rid)
        assertEquals(AnswerProgress.Queued(Action.ACCEPT), progress())
    }

    @Test
    fun aReplyIsNotAnAnswer() {
        open()
        engine.reply(server, rid, "why?")
        assertEquals(AnswerProgress.None, progress())
    }

    @Test
    fun everyStatusHasAWordingOrNothing() {
        for (s in app.portalinbox.proto.AnswerStatus.entries) {
            val p = AnswerProgress.Done(Action.ACCEPT, s.wire)
            assertEquals(s, p.status)
            // No status has an empty message.
            OutboxAnswer.resultLine(p)?.let { assertTrue(it.isNotBlank()) }
        }
    }
}
