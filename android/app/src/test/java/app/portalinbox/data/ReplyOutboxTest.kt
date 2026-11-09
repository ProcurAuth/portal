package app.portalinbox.data

import app.portalinbox.proto.ItemState
import app.portalinbox.proto.ReplyFrame
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ReplyOutboxTest {
    private val server = 1L
    private var now = 1_800_000_000_000L
    private val repo = MemoryItemRepository()
    private var msg = 0
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now }, { "m-%04d".format(++msg) })
    private val rid = Make.hex(0xb)

    private fun open() = engine.onRequest(server, Make.approval(rid, Make.hex(1)))
    private fun entries() = repo.outboxOf(server, rid)

    // ------------------------------------------------------------ the text

    @Test
    fun aTextOfOnlySpacesIsNoMessage() {
        assertEquals(ReplyOutbox.Draft.Refused("Write a message first."), ReplyOutbox.check("   \n "))
        assertEquals(ReplyOutbox.Draft.Refused("Write a message first."), ReplyOutbox.check(""))
    }

    @Test
    fun theTextIsKeptAsTheOwnerWroteIt() {
        val text = "  Why this host?\n\nUse staging_b.  "
        assertEquals(ReplyOutbox.Draft.Ok(text), ReplyOutbox.check(text))
    }

    @Test
    fun theLimitIsEightThousandCharactersCountedAsCodePoints() {
        assertTrue(ReplyOutbox.check("a".repeat(8000)) is ReplyOutbox.Draft.Ok)
        assertTrue(ReplyOutbox.check("a".repeat(8001)) is ReplyOutbox.Draft.Refused)
        // 8000 emoji are 16000 UTF-16 units but 8000 characters.
        assertTrue(ReplyOutbox.check("😀".repeat(8000)) is ReplyOutbox.Draft.Ok)
        assertTrue(ReplyOutbox.check("😀".repeat(8001)) is ReplyOutbox.Draft.Refused)
    }

    @Test
    fun aNulCharacterIsRefused() {
        assertEquals(ReplyOutbox.Draft.Refused("The text has a character that is not allowed."), ReplyOutbox.check("a\u0000b"))
    }

    // ------------------------------------------------------------ the outbox

    @Test
    fun aReplyWaitsInTheOutboxAsAReplyFrameWithAClientMessageId() {
        open()
        val e = (engine.reply(server, rid, "Why this database?") as Submit.Queued).entry
        assertEquals(OutboxType.REPLY, e.type)
        assertEquals(ReplyFrame(rid, "m-0001", "Why this database?"), e.toFrame())
        val r = ReplyOutbox.replies(entries()).single()
        assertEquals("Why this database?", r.text)
        assertEquals(ReplyStatus.Queued, r.status)
        assertEquals(r, ReplyOutbox.pending(entries()))
    }

    @Test
    fun theStatusFollowsTheSessionAndTheResult() {
        open()
        val e = (engine.reply(server, rid, "hello") as Submit.Queued).entry
        assertEquals(ReplyStatus.Queued, ReplyOutbox.replies(entries()).single().status)
        engine.markSent(e)
        assertEquals(ReplyStatus.Sent, ReplyOutbox.replies(entries()).single().status)
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted", "replied"))
        assertEquals(ReplyStatus.Delivered, ReplyOutbox.replies(entries()).single().status)
        assertNull(ReplyOutbox.pending(entries()))
    }

    @Test
    fun theMessageIsSentAgainAfterEachWelcomeUntilTheResultComes() {
        open()
        engine.reply(server, rid, "hello")
        val first = engine.onWelcome(server, Make.welcome())
        assertEquals(1, first.outbox.size)
        engine.markSent(first.outbox[0])
        // The connection dropped before the result: the next welcome sends the same frame again.
        val second = engine.onWelcome(server, Make.welcome())
        assertEquals(first.outbox[0].clientMsgId, second.outbox.single().clientMsgId)
    }

    @Test
    fun aRefusedReplyShowsItsReason() {
        open()
        val e = (engine.reply(server, rid, "late") as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "abandoned", "abandoned"))
        val s = ReplyOutbox.replies(entries()).single().status
        assertEquals(ReplyStatus.Rejected("abandoned"), s)
        assertEquals("Not sent. Too late: the agent no longer answers.", ReplyOutbox.line(s))
    }

    @Test
    fun everyResultHasItsOwnWords() {
        val expected = mapOf(
            "superseded" to "Not sent. Too late: a newer request replaced this one.",
            "expired" to "Not sent. Too late: this request expired.",
            "cancelled" to "Not sent. Too late: the agent cancelled this request.",
            "closed" to "Not sent. This request is closed.",
            "already_answered" to "Not sent. The request was already answered.",
            "ask_back_not_allowed" to "Not sent. The agent does not take questions here.",
            "device_revoked" to "Not sent. This phone is no longer allowed to answer. Add the server again.",
            "settled" to "Not sent. The request ended first.",
            "something_new" to "Not sent.",
        )
        for ((result, line) in expected) assertEquals(result, line, ReplyOutbox.line(ReplyStatus.Rejected(result)))
        assertNull(ReplyOutbox.line(ReplyStatus.Delivered))
    }

    @Test
    fun theLinesOfAMessageOnItsWayAreShort() {
        assertEquals("Saved. It goes out when the phone is online.", ReplyOutbox.line(ReplyStatus.Queued))
        assertEquals("Sent. Waiting for the server.", ReplyOutbox.line(ReplyStatus.Sent))
    }

    @Test
    fun aSettledItemClosesTheMessageThatWaited() {
        open()
        engine.reply(server, rid, "waiting")
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        assertEquals(ReplyStatus.Rejected("settled"), ReplyOutbox.replies(entries()).single().status)
        assertEquals(ItemState.ABANDONED, repo.item(server, rid)!!.state)
    }

    @Test
    fun answersAreNotMessages() {
        open()
        engine.decline(server, rid)
        assertTrue(ReplyOutbox.replies(entries()).isEmpty())
        assertNull(ReplyOutbox.pending(entries()))
    }

    @Test
    fun theOwnerHasOneMessageAtATime() {
        open()
        engine.reply(server, rid, "one")
        val second = engine.reply(server, rid, "two")
        assertEquals(Submit.Refused(Refusal.ALREADY_QUEUED), second)
        assertEquals("Your last message is still on its way.", ReplyOutbox.submitLine(second))
        assertEquals(1, ReplyOutbox.replies(entries()).size)
    }

    @Test
    fun aNewMessageIsPossibleAfterTheResult() {
        open()
        val e = (engine.reply(server, rid, "one") as Submit.Queued).entry
        engine.onAnswerResult(server, Make.result(rid, e.clientMsgId, "accepted", "replied"))
        assertTrue(engine.reply(server, rid, "two") is Submit.Queued)
        assertEquals(listOf("one", "two"), ReplyOutbox.replies(entries()).map { it.text })
    }

    @Test
    fun theRefusalsOfTheEngineHaveWordsForTheOwner() {
        assertNull(ReplyOutbox.submitLine(Submit.Queued(OutboxEntry(1, "m", "r", OutboxType.REPLY, "{}", 0, 0, null, null, null))))
        assertEquals("This request is closed.", ReplyOutbox.submitLine(Submit.Refused(Refusal.NOT_OPEN)))
        assertEquals("The agent does not take questions here.", ReplyOutbox.submitLine(Submit.Refused(Refusal.ASK_BACK_NOT_ALLOWED)))
        assertEquals("This request is gone.", ReplyOutbox.submitLine(Submit.Refused(Refusal.NOT_FOUND)))
    }
}
