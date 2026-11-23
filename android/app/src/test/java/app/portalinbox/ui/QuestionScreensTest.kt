package app.portalinbox.ui

import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.AnswerSheet
import app.portalinbox.data.ItemEngine
import app.portalinbox.proto.RequestKind
import app.portalinbox.testsupport.Make
import app.portalinbox.testsupport.MemoryItemRepository
import app.portalinbox.testsupport.ReferenceEffectHasher
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Which screen opens an item, and the words that a screen reader says on the screen of a question. */
class QuestionScreensTest {
    private val server = 1L
    private val created = 1_788_000_000_000L
    private val now = created + 6 * 86_400_000L
    private val repo = MemoryItemRepository()
    private val engine = ItemEngine(repo, ReferenceEffectHasher(), { now })
    private val rid = Make.hex(0xb)

    @Test
    fun aQuestionOpensTheQuestionScreenAndAnApprovalOpensTheApprovalScreen() {
        assertEquals(ItemScreens.Screen.QUESTION, ItemScreens.of(RequestKind.QUESTION))
        assertEquals(ItemScreens.Screen.APPROVAL, ItemScreens.of(RequestKind.APPROVAL))
    }

    @Test
    fun theHeaderSaysTheKindTheAgentTheAgesAndTheNumberOfOptions() {
        engine.onRequest(server, Make.question(rid, Make.hex(1), createdAtMs = created) { it.put("agent_seen_at_ms", now - 3 * 60_000L) })
        val i = repo.item(server, rid)!!
        assertEquals(
            "Question from the agent triage, Purge the staging data of November?, asked 6 days ago, agent active 3 minutes ago, 2 options",
            TalkBackLabels.questionHeader(i, now, "triage"),
        )
    }

    @Test
    fun aBlockOfAFormIsReadWithItsNumberItsRuleAndItsState() {
        assertEquals("Question 2 of 3, Retention. Choose one. Not answered", TalkBackLabels.block(2, 3, "Retention", "Choose one.", false))
        assertEquals("Choose one. Answered", TalkBackLabels.block(1, 1, null, "Choose one.", true))
    }

    @Test
    fun theSendButtonNamesThePortalAndSaysWhyItIsOff() {
        engine.onRequest(server, Make.question(rid, Make.hex(1), createdAtMs = created))
        val i = repo.item(server, rid)!!
        val sh = AnswerSheet.of(i.request)!!
        fun screen(ids: List<String>) = QuestionState.of(
            QuestionState.Input(i, now, ids.fold(AnswerSheet.empty(sh)) { d, id -> AnswerSheet.tap(sh, d, 0, id) }, AnswerProgress.None, true),
        )
        assertEquals("Send answer on Production", TalkBackLabels.questionSend(screen(listOf("go")), "Production"))
        assertEquals(
            "Send answer on Production. Not available now. Choose an answer to turn Send on.",
            TalkBackLabels.questionSend(screen(emptyList()), "Production"),
        )
        assertEquals("Send answer", TalkBackLabels.questionSend(screen(listOf("go")), null))
    }

    @Test
    fun theOtherRowIsReadWithItsNoteAndTheWarningAgainstASecret() {
        val label = TalkBackLabels.otherRow(QuestionState.OTHER_NOTE)
        assertEquals(
            "Other answer. Your text is the answer. It ends the question. Do not enter a password or a secret. The agent reads this text.",
            label,
        )
        assertTrue(TalkBackLabels.questionField("Other answer").endsWith(QuestionState.SECRET_WARNING))
    }

    @Test
    fun theNotesUnderTheOtherRowFollowTheKindOfBlock() {
        assertEquals("Your own answer. It replaces the ticks.", QuestionState.otherNote(app.portalinbox.data.BlockKind.CHECKBOX, true))
        assertEquals("Your own answer. It replaces the choices.", QuestionState.otherNote(app.portalinbox.data.BlockKind.RADIO, true))
        assertEquals(QuestionState.OTHER_NOTE, QuestionState.otherNote(app.portalinbox.data.BlockKind.RADIO, false))
        assertFalse(QuestionState.OTHER_NOTE.isEmpty())
    }

    @Test
    fun aHiddenCharacterInTheTitleShowsAsAMarkForTheScreenReader() {
        engine.onRequest(server, Make.question(rid, Make.hex(1), createdAtMs = created) { it.put("title", "Run​ it?") })
        val i = repo.item(server, rid)!!
        assertTrue(TalkBackLabels.questionHeader(i, now).contains("<U+200B>"))
    }
}
