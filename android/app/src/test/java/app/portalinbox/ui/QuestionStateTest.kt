package app.portalinbox.ui

import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.AnswerSheet
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.SheetDraft
import app.portalinbox.proto.Action
import app.portalinbox.proto.ItemState
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The screen of a question: which rows and buttons show, and why Send is off. */
class QuestionStateTest {
    private val server = 1L
    private val created = 1_788_000_000_000L
    private var now = created + 6 * 60_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })
    private val rid = Make.hex(0xb)

    private fun item(file: String = "request-question-choice.json", edit: (JSONObject) -> Unit = {}): Item {
        engine.onRequest(server, Make.question(rid, Make.hex(1), file = file, createdAtMs = created, edit = edit))
        return repo.item(server, rid)!!
    }

    private fun screen(
        i: Item,
        draft: SheetDraft? = null,
        answer: AnswerProgress = AnswerProgress.None,
        auth: Boolean = true,
        link: ApprovalState.Link = ApprovalState.Link.ONLINE,
        locked: Boolean = false,
        discussed: Boolean = false,
    ) = QuestionState.of(QuestionState.Input(i, now, draft, answer, auth, link, "Production", locked, discussed))

    private fun ticked(i: Item, vararg ids: String): SheetDraft {
        val s = AnswerSheet.of(i.request)!!
        return ids.fold(AnswerSheet.empty(s)) { d, id -> AnswerSheet.tap(s, d, 0, id) }
    }

    @Test
    fun aConversationalQuestionShowsTheRowsOfItsFlags() {
        val s = screen(item())
        assertEquals(QuestionState.Mode.OPEN, s.mode)
        assertFalse(s.automatic)
        assertEquals("Question", s.kindWord)
        assertTrue(s.showOther)
        assertTrue(s.showAsk)
        assertEquals("Ask the agent a question", s.askWord)
        assertEquals("Billing bot · key of alice · conversational agent", s.senderLine)
        assertEquals("Asked 6 min ago · agent active 6 min ago", s.statusLine)
        assertNull(s.closedLine)
    }

    @Test
    fun aClosedQuestionHasNoEscapeRowAndOnlyDecline() {
        val s = screen(item { it.put("allow_custom", false).put("allow_ask_back", false) })
        assertFalse(s.showOther)
        assertFalse(s.showAsk)
        assertTrue(s.rows.closed)
        assertTrue(s.declineEnabled)
        assertEquals("Decline to answer", QuestionState.DECLINE_WORD)
    }

    @Test
    fun theAskRowSaysOpenTheDiscussionOnceTheAgentIsTalking() {
        val i = item()
        assertEquals("Open the discussion", screen(i, discussed = true).askWord)
    }

    @Test
    fun sendIsOffUntilTheAnswerIsValidAndThenAsksForTheFingerprint() {
        val i = item()
        val empty = screen(i)
        assertTrue(empty.sendShown)
        assertFalse(empty.sendEnabled)
        assertEquals(QuestionState.Lock.NOT_COMPLETE, empty.sendLock)
        assertEquals("Choose an answer to turn Send on.", empty.bottomHint)
        val ready = screen(i, ticked(i, "go"))
        assertTrue(ready.sendEnabled)
        assertNull(ready.sendLock)
        assertEquals("Send asks for your fingerprint or your device PIN.", ready.bottomHint)
        assertEquals("Send answer", ready.sendWord)
    }

    @Test
    fun withoutAScreenLockSendStaysOffAndSaysWhy() {
        val i = item()
        val s = screen(i, ticked(i, "go"), auth = false)
        assertFalse(s.sendEnabled)
        assertEquals(QuestionState.Lock.NO_SCREEN_LOCK, s.sendLock)
        assertEquals("Set a screen lock or a fingerprint on this phone to send an answer.", s.bottomHint)
        assertTrue(s.declineEnabled)
    }

    @Test
    fun anOfflinePortalQueuesTheAnswerAndABlockedOneSendsNothing() {
        val i = item()
        val off = screen(i, ticked(i, "go"), link = ApprovalState.Link.OFFLINE)
        assertTrue(off.sendEnabled)
        assertTrue(off.queued)
        assertEquals("Queue answer", off.sendWord)
        assertEquals("Offline. Your answer is sent when Production is back.", off.linkLine)
        val blocked = screen(i, ticked(i, "go"), link = ApprovalState.Link.BLOCKED)
        assertFalse(blocked.sendEnabled)
        assertEquals(QuestionState.Lock.PORTAL_BLOCKED, blocked.sendLock)
        assertFalse(blocked.declineEnabled)
    }

    @Test
    fun aNewAgentThatTheServerLocksTurnsSendOffButKeepsDeclineAndAsk() {
        // SPEC 3.8.16: the server refuses `accept` for every item of a new agent while `lock_new` is true.
        val i = item()
        val s = screen(i, ticked(i, "go"), locked = true)
        assertFalse(s.sendEnabled)
        assertEquals(QuestionState.Lock.AGENT_LOCKED, s.sendLock)
        assertEquals("Send is off: this agent is new. Name it or allow it first. You can decline now.", s.bottomHint)
        assertTrue(s.declineEnabled)
        assertTrue(s.showAsk)
    }

    @Test
    fun whileAnAnswerIsOnItsWayBothButtonsAreOff() {
        val i = item()
        val s = screen(i, ticked(i, "go"), answer = AnswerProgress.Queued(Action.ACCEPT))
        assertEquals(QuestionState.Mode.SENDING, s.mode)
        assertEquals(QuestionState.Lock.SENDING, s.sendLock)
        assertFalse(s.sendEnabled)
        assertFalse(s.declineEnabled)
        assertFalse(s.showAsk)
        assertEquals("Your answer is saved. It goes out when the phone is online.", s.bottomHint)
    }

    @Test
    fun aMessageWithNoAnswerShowsOnlyDecline() {
        val i = item {
            it.put("allow_custom", false).put("allow_ask_back", false)
            it.put("input", JSONObject().put("type", "none").put("multiple", false).put("min", 0).put("max", 0).put("options", org.json.JSONArray()))
        }
        val s = screen(i)
        assertFalse(s.sendShown)
        assertTrue(s.declineEnabled)
        assertEquals(QuestionState.NO_ANSWER_NOTE, s.bottomHint)
    }

    @Test
    fun aTextQuestionWithAskBackShowsTheFieldAndTheAskRowButNoOtherRow() {
        val i = item("request-question-text-until.json")
        val s = screen(i)
        assertFalse(s.showOther)
        assertTrue(s.showAsk)
        assertTrue(s.sendShown)
        assertEquals("Write your answer to turn Send on.", s.bottomHint)
    }

    @Test
    fun theSuggestionShowsByTheLabelOfTheOptionWithItsConfidenceAndNeverTicksIt() {
        val i = item()
        val s = screen(i)
        val h = s.hint!!
        assertEquals("Run it", h.suggested)
        assertEquals(62, h.percent)
        assertEquals("system_one", h.source)
        assertTrue(h.spoken.contains("confidence 62 percent"))
        assertFalse(s.sendEnabled)
    }

    @Test
    fun aFormShowsTheCountOfAnsweredQuestions() {
        val i = item("request-question-form.json")
        val sh = AnswerSheet.of(i.request)!!
        val d = AnswerSheet.tap(sh, AnswerSheet.empty(sh), 0, "prod")
        val s = screen(i, d)
        assertEquals("Asked 6 min ago · agent active 6 min ago · 1 of 4 answered", s.statusLine)
        assertEquals("Send answers", s.sendWord)
        assertEquals("Send is off. Questions 2, 3 and 4 have no valid answer.", s.bottomHint)
    }

    @Test
    fun aClosedQuestionHasNoButtonToSendAndNoRows() {
        val i = item()
        engine.onSettled(server, Make.settled(rid, Make.hex(2), "settled-abandoned.json"))
        val closed = repo.item(server, rid)!!
        assertEquals(ItemState.ABANDONED, closed.state)
        val s = screen(closed)
        assertEquals(QuestionState.Mode.CLOSED, s.mode)
        assertFalse(s.sendShown)
        assertFalse(s.declineShown)
        assertFalse(s.showAsk)
        assertFalse(s.showOther)
        assertNull(s.linkLine)
        assertNotNull(i)
    }

    @Test
    fun theHiddenCharactersOfTheAgentShowAsMarksInTheSenderLineAndTheSuggestion() {
        val i = item { o ->
            o.getJSONObject("from").put("label", "bill‮ing")
            o.getJSONObject("hint").put("suggested", "go​")
        }
        val s = screen(i)
        assertTrue(s.senderLine.contains("<U+202E>"))
        assertTrue(s.hint!!.suggested.contains("<U+200B>"))
    }

    @Test
    fun theWordsOfTheSecretWarningAreFixed() {
        assertEquals("Do not enter a password or a secret. The agent reads this text.", QuestionState.SECRET_WARNING)
    }
}
