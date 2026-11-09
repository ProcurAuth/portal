package app.portalinbox.ui

import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.Submit
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.MessageFrame
import app.portalinbox.proto.ServerFrames
import app.portalinbox.proto.SettledFrame
import app.portalinbox.proto.StrictJson
import app.portalinbox.testsupport.Fixtures
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.ZoneOffset

class ThreadStateTest {
    private val server = 1L
    private var now = 1_788_000_400_000L
    private val repo = MemoryItemRepository()
    private var msg = 0
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now }, { "m-%04d".format(++msg) })
    private val rid = Make.hex(0xb)

    private fun open(allowAskBack: Boolean = true, edit: (org.json.JSONObject) -> Unit = {}): Item {
        engine.onRequest(server, Make.approval(rid, Make.hex(1), allowAskBack = allowAskBack, edit = edit))
        return repo.item(server, rid)!!
    }

    private fun screen(offsetMs: Long = 0L): ThreadState.Screen {
        val item = repo.item(server, rid)!!
        return ThreadState.of(
            ThreadState.Input(item, repo.turns(server, rid), repo.outboxOf(server, rid), now, offsetMs, ZoneOffset.UTC),
        )
    }

    private fun agentTurn(seq: Long, delivery: Int) = engine.onMessage(server, Make.message(rid, Make.hex(delivery), seq, user = false))
    private fun userTurn(seq: Long, delivery: Int) = engine.onMessage(server, Make.message(rid, Make.hex(delivery), seq, user = true))

    // ------------------------------------------------------------ the thread

    @Test
    fun theTurnsOfTheAgentAndTheOwnerShowInTheOrderOfTheirSeq() {
        open()
        userTurn(2, 0x22)
        agentTurn(1, 0x11)
        val entries = screen().entries
        assertEquals(listOf(1L, 2L), entries.map { (it as? ThreadState.Entry.Agent)?.seq ?: (it as ThreadState.Entry.User).seq })
        assertTrue(entries[0] is ThreadState.Entry.Agent)
        assertTrue(entries[1] is ThreadState.Entry.User)
        assertEquals("The table has 3 rows with this id.", (entries[0] as ThreadState.Entry.Agent).explanation)
        assertEquals("Why not use the staging database?", (entries[1] as ThreadState.Entry.User).text)
    }

    @Test
    fun aMessageOfThisPhoneShowsFromTheOutboxBecauseTheServerDoesNotSendItBack() {
        open()
        agentTurn(1, 0x11)
        val e = (engine.reply(server, rid, "Why?") as Submit.Queued).entry
        engine.markSent(e)
        val s = screen()
        val mine = s.entries.last() as ThreadState.Entry.User
        assertEquals("Why?", mine.text)
        assertNull(mine.seq)
        assertEquals(ThreadState.Delivery.SENT, mine.delivery)
        assertEquals("Sent. Waiting for the server.", mine.line)
    }

    @Test
    fun aMessageOfTheOutboxIsPlacedByTheClockOfTheServer() {
        open()
        agentTurn(1, 0x11) // 1788000100000
        now = 1_788_000_150_000L
        engine.reply(server, rid, "mine")
        // The phone clock is 100 s late: its 1788000150000 is 1788000250000 for the server, after the turn of the agent.
        val s = screen(offsetMs = 100_000)
        assertEquals(listOf("Agent", "User"), s.entries.map { if (it is ThreadState.Entry.Agent) "Agent" else "User" })
        // With the phone clock 200 s early the message was before the turn of the agent.
        val early = screen(offsetMs = -200_000)
        assertEquals(listOf("User", "Agent"), early.entries.map { if (it is ThreadState.Entry.Agent) "Agent" else "User" })
    }

    @Test
    fun aDeliveredMessageThatTheServerAlsoSentBackShowsOnce() {
        open()
        agentTurn(1, 0x11)
        // The outbox entry is 10 s older than the same text from the server: the clock of the phone is right.
        now = 1_788_000_190_000L
        val e = (engine.reply(server, rid, "Why not use the staging database?") as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted", "replied"))
        userTurn(2, 0x22)
        val users = screen().entries.filterIsInstance<ThreadState.Entry.User>()
        assertEquals(1, users.size)
    }

    @Test
    fun everyTurnOfTheAgentCarriesTheMarkdownFlagAndTheCommandOfTheTurn() {
        open()
        val o = Fixtures.golden("server", "message-agent-markdown.json").json.put("request_id", rid).put("id", Make.hex(0x33))
        engine.onMessage(server, ServerFrames.decode(StrictJson.compact(o)) as MessageFrame)
        val a = screen().entries.single() as ThreadState.Entry.Agent
        assertEquals(app.portalinbox.proto.ExplanationFormat.MARKDOWN, a.format)
        assertNotNull(a.explanation)
    }

    @Test
    fun aTurnWithACommandKeepsTheCommandAsItCame() {
        open()
        agentTurn(1, 0x11)
        val a = screen().entries.single() as ThreadState.Entry.Agent
        assertEquals("SELECT * FROM analytics.events WHERE id = 7", a.command!!.text)
    }

    // ------------------------------------------------------------ the field

    @Test
    fun theFieldIsOpenWhenItIsTheTurnOfTheOwner() {
        open()
        val s = screen()
        assertEquals(ThreadState.Phase.YOUR_TURN, s.phase)
        assertTrue(s.composer.enabled)
        assertEquals("Write to the agent", s.composer.placeholder)
        assertFalse(s.writing)
        assertTrue(s.statusLine.endsWith("your turn"))
        assertTrue(s.statusLine.startsWith("agent active "))
    }

    @Test
    fun theFieldIsBlockedWhileTheAgentPreparesItsReply() {
        open()
        val e = (engine.reply(server, rid, "Fine. Run it at night.") as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted", "replied"))
        val s = screen()
        assertEquals(ItemState.REPLIED, repo.item(server, rid)!!.state)
        assertEquals(ThreadState.Phase.WAITING_FOR_AGENT, s.phase)
        assertEquals(ThreadState.Block.WAITING_FOR_AGENT, s.composer.block)
        assertFalse(s.composer.enabled)
        assertTrue(s.writing)
        assertEquals("The agent is writing its answer. You can write again when it replies.", s.composer.hint)
    }

    @Test
    fun theFieldOpensAgainWhenTheAgentReplies() {
        open()
        val e = (engine.reply(server, rid, "why") as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted", "replied"))
        agentTurn(1, 0x11)
        val s = screen()
        assertEquals(ItemState.PENDING, repo.item(server, rid)!!.state)
        assertTrue(s.composer.enabled)
        assertFalse(s.writing)
    }

    @Test
    fun aReplyFromAnotherDeviceBlocksTheFieldToo() {
        open()
        userTurn(1, 0x22)
        val s = screen()
        assertEquals(ThreadState.Phase.WAITING_FOR_AGENT, s.phase)
        assertFalse(s.composer.enabled)
    }

    @Test
    fun theFieldIsBlockedWhileTheMessageWaitsInTheOutbox() {
        open()
        engine.reply(server, rid, "saved offline")
        val queued = screen()
        assertEquals(ThreadState.Block.SENDING, queued.composer.block)
        assertEquals("Your message is saved. It goes out when the phone is online.", queued.composer.hint)
        assertEquals(ThreadState.Delivery.QUEUED, (queued.entries.last() as ThreadState.Entry.User).delivery)
        // The row "is writing" waits for the server to take the message.
        assertFalse(queued.writing)
    }

    @Test
    fun aSecondMessageWhileOneWaitsIsRefusedByTheEngine() {
        open()
        engine.reply(server, rid, "one")
        assertEquals(Submit.Refused(app.portalinbox.data.Refusal.ALREADY_QUEUED), engine.reply(server, rid, "two"))
    }

    @Test
    fun theFieldIsNotThereWhenTheAgentTakesNoQuestions() {
        open(allowAskBack = false)
        val s = screen()
        assertEquals(ThreadState.Block.NOT_ALLOWED, s.composer.block)
        assertFalse(s.composer.enabled)
        assertTrue(s.composer.hint!!.endsWith("does not take questions here."))
    }

    @Test
    fun anAutomaticSystemHasNoDiscussion() {
        engine.onRequest(server, Make.question(rid, Make.hex(1), file = "request-question-confirm-one-shot.json"))
        val s = screen()
        assertEquals(ThreadState.Block.NOT_ALLOWED, s.composer.block)
    }

    // ------------------------------------------------------------ the end states

    private fun settle(file: String) {
        engine.onSettled(server, Make.settled(rid, Make.hex(0x99), file))
    }

    @Test
    fun anAbandonedItemSaysThatTheAgentNoLongerAnswersAndIsNotADecline() {
        open()
        settle("settled-abandoned.json")
        val s = screen()
        assertEquals(ThreadState.Phase.CLOSED, s.phase)
        assertEquals(ThreadState.Block.CLOSED, s.composer.block)
        assertEquals(ThreadState.Chip("abandoned", ThreadState.Tone.WARNING), s.chip)
        assertTrue(s.banner!!.text.startsWith("The agent no longer answers. Last sign "))
        assertTrue(s.banner!!.text.contains("This is not a decline. You cannot write now."))
        assertEquals("", s.statusLine)
    }

    @Test
    fun aReplacedItemLinksToTheNewOne() {
        open()
        settle("settled-superseded.json")
        val s = screen()
        assertEquals(ThreadState.Chip("replaced", ThreadState.Tone.INFO), s.chip)
        assertEquals("e".repeat(32), s.replacedBy)
        assertTrue(s.banner!!.text.startsWith("The agent proposes something else."))
        assertFalse(s.composer.enabled)
    }

    @Test
    fun anExpiredItemSaysThatNoDecisionWasMade() {
        open()
        val o = Fixtures.golden("server", "settled-answered.json").json
        o.put("id", Make.hex(0x98)).put("request_id", rid).put("state", "expired").put("reason", "max_lifetime").remove("decision")
        o.remove("by")
        engine.onSettled(server, ServerFrames.decode(StrictJson.compact(o)) as SettledFrame)
        val s = screen()
        assertEquals("expired", s.chip!!.word)
        assertEquals("Life limit reached. No decision was made. The request is closed.", s.banner!!.text)
        assertNull(s.replacedBy)
    }

    @Test
    fun aReplyThatCameTooLateShowsAsNotSentUnderItsBubble() {
        open()
        val e = (engine.reply(server, rid, "Do it tonight") as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "abandoned", "abandoned"))
        val s = screen()
        val mine = s.entries.last() as ThreadState.Entry.User
        assertEquals(ThreadState.Delivery.NOT_SENT, mine.delivery)
        assertEquals("Not sent. Too late: the agent no longer answers.", mine.line)
        assertEquals(ThreadState.Phase.CLOSED, s.phase)
    }

    @Test
    fun aMessageThatAnEndOfTheItemClosedIsNotSent() {
        open()
        engine.reply(server, rid, "pending when it ended")
        settle("settled-abandoned.json")
        val mine = screen().entries.last() as ThreadState.Entry.User
        assertEquals(ThreadState.Delivery.NOT_SENT, mine.delivery)
        assertEquals("Not sent. The request ended first.", mine.line)
    }

    @Test
    fun anAnsweredItemClosesTheDiscussionWithTheWordsOfTheAnswer() {
        open()
        settle("settled-answered.json")
        val s = screen()
        assertTrue(s.banner!!.text.endsWith("The discussion is closed."))
        assertFalse(s.composer.enabled)
    }

    // ------------------------------------------------------------ names and marks

    @Test
    fun theAgentNameShowsItsHiddenCharactersAsMarks() {
        open { it.getJSONObject("from").put("label", "ops‮agent") }
        assertEquals("ops<U+202E>agent", screen().agentLabel)
    }

    @Test
    fun aQuestionOpensTheThreadWithItsTitleAndItsChoices() {
        engine.onRequest(server, Make.question(rid, Make.hex(1)))
        val s = screen()
        assertEquals("Purge the staging data of November?", s.opening!!.title)
        assertEquals(listOf("Run it", "Wait"), s.opening!!.options.take(2))
    }
}
