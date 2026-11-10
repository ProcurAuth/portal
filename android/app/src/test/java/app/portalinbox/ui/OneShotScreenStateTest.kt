package app.portalinbox.ui

import app.portalinbox.data.AnswerSheet
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEngine
import app.portalinbox.data.Refusal
import app.portalinbox.data.Submit
import app.portalinbox.proto.FrameException
import app.portalinbox.proto.Interaction
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/** DIAL 1.6 and 4.3 (mock 3c): the screen of an automatic system that holds no conversation. */
class OneShotScreenStateTest {
    private val server = 1L
    private val created = 1_788_000_000_000L
    private var now = created + 12 * 60_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })
    private val rid = Make.hex(0xb)

    private fun opt(id: String, description: String? = null) =
        JSONObject().put("id", id).put("label", id).also { if (description != null) it.put("description", description) }

    /** The decision of mock 3c: three teams, a suggestion with 62 %. */
    private fun decision(): Item {
        engine.onRequest(
            server,
            Make.question(rid, Make.hex(1), file = "request-question-confirm-one-shot.json", createdAtMs = created) { o ->
                o.put("title", "Which team handles \"double debit\"?")
                o.getJSONObject("from").put("label", "jev-escalade")
                o.put(
                    "input",
                    JSONObject().put("type", "choice").put("multiple", false).put("min", 1).put("max", 1).put(
                        "options", JSONArray().put(opt("billing", "Charges and refunds")).put(opt("support", "Customer help")).put(opt("infra", "Servers and network")),
                    ),
                )
                o.put("hint", JSONObject().put("suggested", "billing").put("confidence", 0.62).put("source", "jev"))
            },
        )
        return repo.item(server, rid)!!
    }

    private fun screen(i: Item, canAuth: Boolean = true) =
        QuestionState.of(QuestionState.Input(i, now, AnswerSheet.of(i.request)?.let { AnswerSheet.empty(it) }, app.portalinbox.data.AnswerProgress.None, canAuth))

    @Test
    fun theHeaderNamesTheAutomaticSystemWithNoConversation() {
        val i = decision()
        assertEquals(Interaction.ONE_SHOT, i.request.from.interaction)
        val s = screen(i)
        assertTrue(s.automatic)
        assertEquals("Decision", s.kindWord)
        assertEquals("Automatic system (no conversation)", s.typeLabel)
        assertEquals("Automatic system (no conversation) · jev-escalade · key of alice", s.senderLine)
        assertEquals("Asked 12 min ago · last computed 12 min ago", s.statusLine)
        assertEquals("asked 12 minutes ago, last computed 12 minutes ago", s.statusSpoken)
    }

    @Test
    fun theSuggestionAndItsConfidenceShowAboveTheOptionsAndAreNotChecked() {
        val s = screen(decision())
        val h = s.hint!!
        assertEquals("billing", h.suggested)
        assertEquals(62, h.percent)
        assertEquals("jev", h.source)
        assertTrue(h.note.startsWith("Not checked."))
        // The suggestion ticks nothing: Send stays off until the owner chooses.
        assertFalse(s.sendEnabled)
        assertEquals(QuestionState.Lock.NOT_COMPLETE, s.sendLock)
    }

    @Test
    fun thereIsNoOtherRowNoAskRowAndNoDiscussionControl() {
        val s = screen(decision())
        assertFalse(s.showOther)
        assertFalse(s.showAsk)
        assertTrue(s.rows.closed)
        assertEquals(QuestionState.NO_DISCUSSION, s.closedLine)
        assertEquals("This system cannot read free text. You can only choose or decline. There is no discussion.", s.closedLine)
        // The discussion screen says the same: no field for an item of this sender.
        val thread = ThreadState.of(ThreadState.Input(decision(), emptyList(), emptyList(), now))
        assertEquals(ThreadState.Block.NOT_ALLOWED, thread.composer.block)
        assertFalse(thread.composer.enabled)
    }

    @Test
    fun theOwnerCanStillChooseAndDecline() {
        val i = decision()
        val sh = AnswerSheet.of(i.request)!!
        val d = AnswerSheet.tap(sh, AnswerSheet.empty(sh), 0, "infra")
        val s = QuestionState.of(QuestionState.Input(i, now, d, app.portalinbox.data.AnswerProgress.None, true))
        assertTrue(s.sendEnabled)
        assertTrue(s.declineEnabled)
        assertEquals("Send asks for your fingerprint or your device PIN.", s.bottomHint)
        assertTrue(engine.answerQuestion(server, rid, d) is Submit.Queued)
    }

    @Test
    fun aConfirmDecisionIsClosedToo() {
        engine.onRequest(server, Make.question(rid, Make.hex(1), file = "request-question-confirm-one-shot.json", createdAtMs = created))
        val i = repo.item(server, rid)!!
        val s = screen(i)
        assertTrue(s.rows.closed)
        assertNull(s.hint)
        assertEquals(listOf("yes", "no"), AnswerSheet.of(i.request)!!.blocks[0].options.map { it.id })
        // A confirm with no suggestion: nothing above the options.
    }

    @Test
    fun aFrameOfAnAutomaticSystemWithAFlagIsRefusedByTheReader() {
        assertThrows(FrameException::class.java) {
            Make.question(rid, Make.hex(1), file = "request-question-confirm-one-shot.json") { it.put("allow_custom", true) }
        }
        assertThrows(FrameException::class.java) {
            Make.question(rid, Make.hex(1), file = "request-question-confirm-one-shot.json") { it.put("allow_ask_back", true) }
        }
    }

    @Test
    fun theEngineNeverAsksTheAgentOfAnAutomaticSystem() {
        decision()
        val r = engine.reply(server, rid, "why?") as Submit.Refused
        assertEquals(Refusal.ASK_BACK_NOT_ALLOWED, r.reason)
    }

    @Test
    fun theScreenReaderHearsADecisionFromTheAutomaticSystem() {
        val i = decision()
        assertEquals(
            "Decision from the automatic system jev-escalade, Which team handles \"double debit\"?, asked 12 minutes ago, " +
                "last computed 12 minutes ago, 3 options",
            TalkBackLabels.questionHeader(i, now, i.request.from.label),
        )
    }
}
