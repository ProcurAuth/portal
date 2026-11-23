package app.portalinbox.ui

import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.AnswerSheet
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.OutboxType
import app.portalinbox.data.Submit
import app.portalinbox.proto.Action
import app.portalinbox.proto.ItemState
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneId

/** How a question ended: answered, declined, abandoned, replaced, expired, cancelled. */
class QuestionHistoryTest {
    private val server = 1L
    private val created = 1_788_000_000_000L
    private var now = created + 4 * 86_400_000L + 5_000L
    private val repo = MemoryItemRepository()
    private var msg = 0
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now }, { "m-%04d".format(++msg) })
    private val rid = Make.hex(0xb)
    private val zone = ZoneId.of("UTC")

    private fun open(file: String = "request-question-choice.json", edit: (JSONObject) -> Unit = {}): Item {
        engine.onRequest(server, Make.question(rid, Make.hex(1), file = file, createdAtMs = created, edit = edit))
        return repo.item(server, rid)!!
    }

    private fun item() = repo.item(server, rid)!!

    private fun own() = QuestionHistory.own(repo.outboxOf(server, rid))

    private fun sheetOf(i: Item) = AnswerSheet.of(i.request)

    private fun answerWith(vararg ids: String): String {
        val i = item()
        val sh = AnswerSheet.of(i.request)!!
        val d = ids.fold(AnswerSheet.empty(sh)) { acc, id -> AnswerSheet.tap(sh, acc, 0, id) }
        val e = (engine.answerQuestion(server, rid, d) as Submit.Queued).entry
        return e.clientMsgId
    }

    @Test
    fun anAnswerOfThisPhoneShowsWhatWasChosen() {
        open()
        val id = answerWith("go")
        // Before the result there is no own answer: the server did not take it.
        assertNull(own())
        engine.onAnswerResult(server, Make.result(rid, id, "accepted"))
        val o = own()!!
        assertEquals(Action.ACCEPT, o.frame.action)
        assertEquals("Run it", QuestionHistory.summary(sheetOf(item()), o))
        assertEquals("answered", QuestionHistory.word(item(), o))
        assertEquals("Answered: Run it · by you", QuestionHistory.reasonLine(item(), o, sheetOf(item())))
        val banner = QuestionHistory.banner(item(), o, zone, now)!!
        assertTrue(banner.text.startsWith("You answered at "))
        assertEquals(ThreadState.Tone.MUTED, banner.tone)
    }

    @Test
    fun aDeclineOfThisPhoneIsDeclinedNotAnswered() {
        open()
        val e = (engine.decline(server, rid) as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted"))
        val o = own()!!
        assertTrue(o.declined)
        assertEquals("declined", QuestionHistory.word(item(), o))
        assertEquals("Declined by you", QuestionHistory.reasonLine(item(), o, sheetOf(item())))
        assertTrue(QuestionHistory.banner(item(), o, zone, now)!!.text.startsWith("You declined to answer at "))
        assertEquals("Declined", QuestionHistory.summary(sheetOf(item()), o))
    }

    @Test
    fun anAnswerFromTheWebOrAnotherPhoneNamesWhoAnsweredButNotWhat() {
        open()
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-answered.json"))
        val i = item()
        assertEquals(ItemState.ANSWERED, i.state)
        assertNull(own())
        assertEquals("Answered on Pixel 9", QuestionHistory.reasonLine(i, null, sheetOf(i)))
        assertEquals("answered", QuestionHistory.word(i, null))
        assertTrue(QuestionHistory.banner(i, null, zone, now)!!.text.endsWith("The answer is not shown on this phone."))
    }

    @Test
    fun theAnswerBlocksOfAFormListTheChosenAndTheNotChosen() {
        val i = open("request-question-form.json")
        val sh = AnswerSheet.of(i.request)!!
        var d = AnswerSheet.empty(sh)
        d = AnswerSheet.tap(sh, d, 0, "prod")
        d = AnswerSheet.tap(sh, d, 1, "unit")
        d = AnswerSheet.tap(sh, d, 1, "sec")
        d = AnswerSheet.typeOther(sh, d, 2, "45 days")
        d = AnswerSheet.typeText(sh, d, 3, "Every night")
        val e = (engine.answerQuestion(server, rid, d) as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted"))
        val o = own()!!
        val blocks = QuestionHistory.answerBlocks(sh, o.frame)
        assertEquals(4, blocks.size)
        assertEquals(listOf("prod"), blocks[0].chosen.map { it.id })
        assertEquals(listOf("unit", "sec"), blocks[1].chosen.map { it.id })
        assertEquals(listOf("e2e", "load"), blocks[1].notChosen.map { it.id })
        assertEquals("45 days", blocks[2].free)
        assertTrue(blocks[2].chosen.isEmpty())
        assertEquals("Every night", blocks[3].free)
        assertEquals("4 questions answered", QuestionHistory.summary(sh, o))
    }

    @Test
    fun aFreeAnswerShowsBetweenQuotesAndIsShortened() {
        open()
        val i = item()
        val sh = AnswerSheet.of(i.request)!!
        val long = "word ".repeat(30)
        val e = (engine.answerQuestion(server, rid, AnswerSheet.typeOther(sh, AnswerSheet.empty(sh), 0, long)) as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted"))
        val line = QuestionHistory.summary(sh, own()!!)
        assertTrue(line.startsWith("“word word"))
        assertTrue(line.endsWith("…”"))
    }

    // ------------------------------------------------------------ the end states

    @Test
    fun anAbandonedQuestionTellsThatItIsNotADecline() {
        open()
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        val i = item()
        val b = QuestionHistory.banner(i, null, zone, now)!!
        assertTrue(b.text.startsWith("The agent no longer answers. Last sign 4 d ago ("))
        assertTrue(b.text.endsWith("This is not a decline. You cannot answer now."))
        assertEquals(ThreadState.Tone.WARNING, b.tone)
        assertEquals("abandoned", QuestionHistory.word(i, null))
        assertEquals("The agent no longer answers", QuestionHistory.reasonLine(i, null, sheetOf(i)))
    }

    @Test
    fun aReplacedQuestionSaysThatANewQuestionReplacesIt() {
        open()
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-superseded.json"))
        val i = item()
        assertEquals(
            "The agent proposes something else. This question is closed. A new question replaces it.",
            QuestionHistory.banner(i, null, zone, now)!!.text,
        )
        assertEquals("replaced", QuestionHistory.word(i, null))
    }

    @Test
    fun anExpiredQuestionGivesTheReasonAndNoDecision() {
        open()
        val o = Make.settled(rid, Make.hex(2), "settled-abandoned.json")
        val json = o.toJson().put("state", "expired").put("reason", "max_lifetime")
        engine.onSettled(server, app.portalinbox.proto.ServerFrames.decode(app.portalinbox.proto.StrictJson.compact(json)) as app.portalinbox.proto.SettledFrame)
        val i = item()
        assertEquals(ItemState.EXPIRED, i.state)
        assertEquals("Life limit reached. No decision was made. The question is closed.", QuestionHistory.banner(i, null, zone, now)!!.text)
        assertEquals("expired", QuestionHistory.word(i, null))
    }

    @Test
    fun aCancelledQuestionSaysSo() {
        open()
        val json = Make.settled(rid, Make.hex(2), "settled-abandoned.json").toJson().put("state", "cancelled").put("reason", "agent_cancelled")
        engine.onSettled(server, app.portalinbox.proto.ServerFrames.decode(app.portalinbox.proto.StrictJson.compact(json)) as app.portalinbox.proto.SettledFrame)
        assertEquals("The agent cancelled this question.", QuestionHistory.banner(item(), null, zone, now)!!.text)
    }

    @Test
    fun anOpenQuestionHasNoBanner() {
        val i = open()
        assertNull(QuestionHistory.banner(i, null, zone, now))
    }

    // ------------------------------------------------------------ the late answer

    @Test
    fun aLateAnswerGetsTheWordsOfMock6b() {
        open()
        val id = answerWith("go")
        engine.onAnswerResult(server, Make.result(rid, id, "abandoned", "abandoned"))
        val progress = app.portalinbox.data.OutboxAnswer.progress(repo.outboxOf(server, rid)) as AnswerProgress.Done
        assertEquals("Too late: the agent no longer answers. Your answer was not used.", QuestionHistory.resultLine(progress))
        assertEquals(ItemState.ABANDONED, item().state)
        assertNull(own())
    }

    @Test
    fun everyStatusHasWordsForAQuestionAndNoneSaysApproval() {
        for (status in app.portalinbox.proto.AnswerStatus.entries) {
            val line = QuestionHistory.resultLine(AnswerProgress.Done(Action.ACCEPT, status.wire))
            if (line != null) {
                assertTrue("$status: $line", !line.contains("approv", ignoreCase = true) && !line.contains("request"))
            }
        }
        assertEquals("Your answer was sent.", QuestionHistory.resultLine(AnswerProgress.Done(Action.ACCEPT, "accepted")))
        assertEquals("You declined to answer.", QuestionHistory.resultLine(AnswerProgress.Done(Action.DECLINE, "accepted")))
        assertNull(QuestionHistory.resultLine(AnswerProgress.Done(Action.ACCEPT, ItemEngine.RESULT_SETTLED)))
    }

    @Test
    fun theDoneRowOfAQuestionUsesTheOwnAnswer() {
        open()
        val id = answerWith("wait")
        engine.onAnswerResult(server, Make.result(rid, id, "accepted"))
        val row = ItemListOrder.doneRow(item(), own())
        assertEquals("answered", row.word)
        assertEquals("Answered: Wait · by you", row.detail)
        assertTrue(row.openable)
        val declined = open2()
        assertEquals("declined", declined.word)
    }

    private fun open2(): ItemListOrder.DoneRow {
        val other = Make.hex(0xc)
        engine.onRequest(server, Make.question(other, Make.hex(5), createdAtMs = created))
        val e = (engine.decline(server, other) as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(other, e.clientMsgId, "accepted"))
        val i = repo.item(server, other)!!
        return ItemListOrder.doneRow(i, QuestionHistory.own(repo.outboxOf(server, other)))
    }

    @Test
    fun thePromptTitleIsOneLineOfAtMostEightyCharacters() {
        assertEquals("Send answer on Production", BiometricPolicy.answerTitle("Production"))
        assertEquals("Purge staging?", BiometricPolicy.answerSubtitle("Purge staging?"))
        assertEquals("a<U+202E>b", BiometricPolicy.answerSubtitle("a‮b"))
        val long = BiometricPolicy.answerSubtitle("x".repeat(200))
        assertEquals(80, long.codePointCount(0, long.length))
        assertTrue(long.endsWith("…"))
        assertNotNull(OutboxType.ANSWER)
    }
}
