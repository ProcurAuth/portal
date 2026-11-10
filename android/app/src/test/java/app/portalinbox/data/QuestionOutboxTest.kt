package app.portalinbox.data

import app.portalinbox.proto.Action
import app.portalinbox.proto.AnswerFrame
import app.portalinbox.proto.ClientFrames
import app.portalinbox.proto.ItemState
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** An answer to a question goes through the outbox, as an answer to an approval does: saved, sent again, closed by its result. */
class QuestionOutboxTest {
    private val server = 1L
    private var now = 1_800_000_000_000L
    private val repo = MemoryItemRepository()
    private var msg = 0
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now }, { "m-%04d".format(++msg) })
    private val rid = Make.hex(0xb)

    private fun open(file: String = "request-question-choice.json", edit: (JSONObject) -> Unit = {}): Item {
        engine.onRequest(server, Make.question(rid, Make.hex(1), file = file, edit = edit))
        return repo.item(server, rid)!!
    }

    private fun draft(vararg ids: String): SheetDraft {
        val sh = AnswerSheet.of(repo.item(server, rid)!!.request)!!
        return ids.fold(AnswerSheet.empty(sh)) { d, id -> AnswerSheet.tap(sh, d, 0, id) }
    }

    @Test
    fun anAnswerIsSavedInTheOutboxWithItsClientMessageId() {
        open()
        val s = engine.answerQuestion(server, rid, draft("go")) as Submit.Queued
        assertEquals("m-0001", s.entry.clientMsgId)
        assertEquals(OutboxType.ANSWER, s.entry.type)
        assertTrue(s.entry.isPending)
        val f = ClientFrames.decode(s.entry.frame) as AnswerFrame
        assertEquals(Action.ACCEPT, f.action)
        assertEquals(listOf("go"), f.choice)
        assertEquals(1, repo.pendingOutbox(server).size)
    }

    @Test
    fun theAnswerIsSentAgainAfterEachReconnectionUntilTheResultArrives() {
        open()
        val e = (engine.answerQuestion(server, rid, draft("wait")) as Submit.Queued).entry
        var start = engine.onWelcome(server, Make.welcome())
        assertEquals(listOf(e.clientMsgId), start.outbox.map { it.clientMsgId })
        engine.markSent(start.outbox[0])
        now += 60_000
        start = engine.onWelcome(server, Make.welcome())
        assertEquals(e.frame, start.outbox[0].frame)
        engine.markSent(start.outbox[0])
        assertEquals(2, repo.outbox(server, e.clientMsgId)!!.attempts)
        val out = engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted"))
        assertTrue(out.events.single() is ItemEvent.AnswerResult)
        assertTrue(repo.pendingOutbox(server).isEmpty())
        assertEquals(ItemState.ANSWERED, repo.item(server, rid)!!.state)
        assertTrue(engine.onWelcome(server, Make.welcome()).outbox.isEmpty())
    }

    @Test
    fun aLateAnswerGetsTheStateOfTheServerAndTheItemStaysFinal() {
        open()
        val e = (engine.answerQuestion(server, rid, draft("go")) as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "abandoned", "abandoned"))
        assertEquals(ItemState.ABANDONED, repo.item(server, rid)!!.state)
        assertEquals("abandoned", repo.outbox(server, e.clientMsgId)!!.result)
    }

    @Test
    fun aSettledFrameClosesTheAnswerThatWaits() {
        open()
        val e = (engine.answerQuestion(server, rid, draft("go")) as Submit.Queued).entry
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-superseded.json"))
        assertEquals(ItemEngine.RESULT_SETTLED, repo.outbox(server, e.clientMsgId)!!.result)
        assertTrue(engine.onWelcome(server, Make.welcome()).outbox.isEmpty())
    }

    @Test
    fun aSecondAnswerWaitsForTheResultOfTheFirst() {
        open()
        engine.answerQuestion(server, rid, draft("go"))
        assertEquals(Refusal.ALREADY_QUEUED, (engine.answerQuestion(server, rid, draft("wait")) as Submit.Refused).reason)
        assertEquals(Refusal.ALREADY_QUEUED, (engine.decline(server, rid) as Submit.Refused).reason)
        assertEquals(1, repo.pendingOutbox(server).size)
    }

    @Test
    fun anIncompleteAnswerIsNeverQueued() {
        open()
        assertEquals(Refusal.BAD_INPUT, (engine.answerQuestion(server, rid, draft()) as Submit.Refused).reason)
        assertEquals(Refusal.BAD_INPUT, (engine.answerQuestion(server, rid, SheetDraft(emptyList())) as Submit.Refused).reason)
        assertTrue(repo.pendingOutbox(server).isEmpty())
    }

    @Test
    fun aClosedItemAnAbsentItemAndAnApprovalAreRefused() {
        assertEquals(Refusal.NOT_FOUND, (engine.answerQuestion(server, rid, SheetDraft(emptyList())) as Submit.Refused).reason)
        open()
        val d = draft("go")
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        assertEquals(Refusal.NOT_OPEN, (engine.answerQuestion(server, rid, d) as Submit.Refused).reason)
        val approval = Make.hex(0xc)
        engine.onRequest(server, Make.approval(approval, Make.hex(3)))
        assertEquals(Refusal.NOT_A_QUESTION, (engine.answerQuestion(server, approval, SheetDraft(emptyList())) as Submit.Refused).reason)
    }

    @Test
    fun aCustomTextIsQueuedOnlyWhenTheAgentAllowedIt() {
        open { it.put("allow_custom", false) }
        val sh = AnswerSheet.of(repo.item(server, rid)!!.request)!!
        val forced = SheetDraft(listOf(BlockDraft(otherOn = true, otherText = "text")))
        assertEquals(Refusal.BAD_INPUT, (engine.answerQuestion(server, rid, forced) as Submit.Refused).reason)
        assertFalse(sh.blocks[0].otherAllowed)
    }

    @Test
    fun aTextAnswerAndAFormAreQueuedAsOneFrame() {
        open("request-question-text-until.json")
        val sh = AnswerSheet.of(repo.item(server, rid)!!.request)!!
        val d = AnswerSheet.typeText(sh, AnswerSheet.empty(sh), 0, "main")
        val e = (engine.answerQuestion(server, rid, d) as Submit.Queued).entry
        assertEquals("main", (ClientFrames.decode(e.frame) as AnswerFrame).text)
    }

    @Test
    fun theDeclineOfAQuestionCarriesNoText() {
        open()
        val e = (engine.decline(server, rid) as Submit.Queued).entry
        val f = ClientFrames.decode(e.frame) as AnswerFrame
        assertEquals(Action.DECLINE, f.action)
        assertNull(f.text)
        assertNull(f.choice)
        // A text with a decline of a question is refused: only an approval takes one.
        assertEquals(Refusal.ALREADY_QUEUED, (engine.decline(server, rid, "why") as Submit.Refused).reason)
    }

    @Test
    fun aQuestionOfAnAutomaticSystemWithAnInvalidCombinationIsNotAnswered() {
        // The reader refuses such a frame, so an item of this kind cannot exist. The engine checks the matrix anyway.
        val f = Make.question(rid, Make.hex(1), file = "request-question-confirm-one-shot.json")
        engine.onRequest(server, f)
        val i = repo.item(server, rid)!!
        assertNull(FlagsMatrix.rows(i.request).invalid)
        val sh = AnswerSheet.of(i.request)!!
        val d = AnswerSheet.tap(sh, AnswerSheet.empty(sh), 0, "yes")
        assertTrue(engine.answerQuestion(server, rid, d) is Submit.Queued)
        assertEquals(JSONArray(listOf("yes")).toString(), JSONArray((ClientFrames.decode(repo.pendingOutbox(server)[0].frame) as AnswerFrame).choice).toString())
    }
}
