package app.portalinbox.data

import app.portalinbox.proto.Action
import app.portalinbox.proto.AnswerFrame
import app.portalinbox.proto.ClientFrames
import app.portalinbox.proto.FormAnswer
import app.portalinbox.proto.FrameException
import app.portalinbox.testsupport.Make
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/** The `answer` frame of a question: choice, custom text, decline, confirm, text, forms. The rules are those of the server. */
class AnswerFrameBuildTest {
    private val rid = Make.hex(0xb)
    private val msg = "m-0001"

    private fun sheetOf(file: String = "request-question-choice.json", edit: (JSONObject) -> Unit = {}): Sheet =
        AnswerSheet.of(Make.question(rid, Make.hex(1), file = file, edit = edit))!!

    private fun build(s: Sheet, d: SheetDraft): Built = AnswerSheet.build(s, d, rid, msg)

    private fun frame(b: Built): AnswerFrame = (b as Built.Frame).frame

    /** The frame must pass the strict encoder of the phone and the strict reader: it is what the server reads. */
    private fun roundTrip(f: AnswerFrame): AnswerFrame = ClientFrames.decode(String(ClientFrames.encode(f), Charsets.UTF_8)) as AnswerFrame

    // ------------------------------------------------------------ choice

    @Test
    fun aChoiceAnswerCarriesTheIdOfTheOption() {
        val s = sheetOf()
        val d = AnswerSheet.tap(s, AnswerSheet.empty(s), 0, "go")
        val f = frame(build(s, d))
        assertEquals(Action.ACCEPT, f.action)
        assertEquals(listOf("go"), f.choice)
        assertNull(f.text)
        assertNull(f.answers)
        assertNull(f.shownEffect)
        assertNull(f.intent)
        assertEquals(rid, f.requestId)
        assertEquals(msg, f.clientMsgId)
        assertEquals(f, roundTrip(f))
    }

    @Test
    fun theTicksOfAMultipleChoiceGoInTheOrderOfTheOptions() {
        val s = sheetOf { o ->
            o.getJSONObject("input").put("multiple", true).put("min", 1).put("max", 3).put(
                "options",
                JSONArray().put(JSONObject().put("id", "a").put("label", "A")).put(JSONObject().put("id", "b").put("label", "B"))
                    .put(JSONObject().put("id", "c").put("label", "C")),
            )
        }
        var d = AnswerSheet.empty(s)
        for (id in listOf("c", "a")) d = AnswerSheet.tap(s, d, 0, id)
        assertEquals(listOf("a", "c"), frame(build(s, d)).choice)
        assertEquals(frame(build(s, d)), roundTrip(frame(build(s, d))))
    }

    @Test
    fun noTickGivesNoFrame() {
        val s = sheetOf()
        val r = build(s, AnswerSheet.empty(s)) as Built.Refused
        assertEquals(listOf(0), r.blocks)
        assertEquals("Choose an answer to turn Send on.", r.message)
    }

    // ------------------------------------------------------------ custom text

    @Test
    fun aCustomAnswerCarriesTheTextAndNoChoice() {
        val s = sheetOf()
        val d = AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, "Not before 18:00. Ask me again then.")
        val f = frame(build(s, d))
        assertEquals(Action.ACCEPT, f.action)
        assertNull(f.choice)
        assertEquals("Not before 18:00. Ask me again then.", f.text)
        assertEquals(f, roundTrip(f))
    }

    @Test
    fun theTextIsNotChangedAndATextOfOnlySpacesIsNoAnswer() {
        val s = sheetOf()
        val spaced = "  two  spaces\nand a line  "
        assertEquals(spaced, frame(build(s, AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, spaced))).text)
        assertTrue(build(s, AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, "   \n ")) is Built.Refused)
    }

    @Test
    fun theLimitsOfTheTextAreThoseOfTheServer() {
        val s = sheetOf()
        val max = "x".repeat(AnswerSheet.MAX_TEXT_CHARS)
        assertEquals(max, frame(build(s, AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, max))).text)
        val over = AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, max + "x")
        assertEquals(BlockStatus.Invalid("The text is too long. Use 8000 characters at most."), AnswerSheet.status(s, over, 0))
        assertTrue(build(s, over) is Built.Refused)
        // A pair of surrogates counts as one character, as in the core crate.
        val emoji = "😀".repeat(AnswerSheet.MAX_TEXT_CHARS)
        assertTrue(build(s, AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, emoji)) is Built.Frame)
        val nul = AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, "a\u0000b")
        assertEquals(BlockStatus.Invalid("The text has a character that is not allowed."), AnswerSheet.status(s, nul, 0))
    }

    @Test
    fun withoutAllowCustomNoFrameHasText() {
        val s = sheetOf { it.put("allow_custom", false) }
        val d = AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, "text")
        assertTrue(build(s, d) is Built.Refused)
    }

    // ------------------------------------------------------------ decline

    @Test
    fun aDeclineCarriesNoChoiceNoTextNoIntent() {
        val f = AnswerSheet.decline(rid, msg)
        assertEquals(Action.DECLINE, f.action)
        assertNull(f.choice)
        assertNull(f.text)
        assertNull(f.intent)
        assertNull(f.answers)
        assertNull(f.shownEffect)
        assertEquals(f, roundTrip(f))
        assertEquals(
            """{"action":"decline","client_msg_id":"$msg","request_id":"$rid","t":"answer"}""",
            app.portalinbox.proto.StrictJson.compact(f.toJson()),
        )
    }

    @Test
    fun aDeclineWithAChoiceOrAnAnswersListIsNotAFrame() {
        assertThrows(FrameException::class.java) { ClientFrames.encode(AnswerFrame(rid, msg, Action.DECLINE, choice = listOf("go"))) }
        assertThrows(FrameException::class.java) {
            ClientFrames.encode(AnswerFrame(rid, msg, Action.DECLINE, answers = listOf(FormAnswer("a", text = "x"))))
        }
    }

    // ------------------------------------------------------------ confirm and text

    @Test
    fun aConfirmAnswerIsYesOrNo() {
        val s = sheetOf("request-question-confirm-one-shot.json")
        assertEquals(BlockKind.CONFIRM, s.blocks[0].kind)
        val yes = frame(build(s, AnswerSheet.tap(s, AnswerSheet.empty(s), 0, "yes")))
        assertEquals(listOf("yes"), yes.choice)
        val no = frame(build(s, AnswerSheet.tap(s, AnswerSheet.empty(s), 0, "no")))
        assertEquals(listOf("no"), no.choice)
        assertEquals(no, roundTrip(no))
        // A tap on something else changes nothing.
        assertEquals(AnswerSheet.empty(s), AnswerSheet.tap(s, AnswerSheet.empty(s), 0, "maybe"))
    }

    @Test
    fun aTextAnswerCarriesTheText() {
        val s = sheetOf("request-question-text-until.json")
        assertEquals(BlockKind.TEXT, s.blocks[0].kind)
        assertEquals("branch name", s.blocks[0].placeholder)
        val f = frame(build(s, AnswerSheet.typeText(s, AnswerSheet.empty(s), 0, "release/5.2")))
        assertEquals("release/5.2", f.text)
        assertNull(f.choice)
        assertEquals(f, roundTrip(f))
        assertTrue(build(s, AnswerSheet.empty(s)) is Built.Refused)
    }

    @Test
    fun aMessageWithNoAnswerHasNoSheet() {
        val f = Make.question(rid, Make.hex(1)) { o ->
            o.put("allow_custom", false).put("allow_ask_back", false)
            o.put("input", JSONObject().put("type", "none").put("multiple", false).put("min", 0).put("max", 0).put("options", JSONArray()))
        }
        assertNull(AnswerSheet.of(f))
    }

    // ------------------------------------------------------------ forms

    @Test
    fun aFormAnswerHasOneEntryForEachQuestion() {
        val s = sheetOf("request-question-form.json")
        assertEquals(4, s.blocks.size)
        assertTrue(s.isForm)
        var d = AnswerSheet.empty(s)
        d = AnswerSheet.tap(s, d, 0, "prod")
        d = AnswerSheet.tap(s, d, 1, "e2e")
        d = AnswerSheet.tap(s, d, 1, "unit")
        d = AnswerSheet.typeOther(s, d, 2, "after the freeze")
        // The form is not complete: the text question has no answer yet.
        val r = build(s, d) as Built.Refused
        assertEquals(listOf(3), r.blocks)
        assertEquals("Send is off. Question 4 has no valid answer.", r.message)
        d = AnswerSheet.typeText(s, d, 3, "No secret here")
        val f = frame(build(s, d))
        assertNull(f.choice)
        assertNull(f.text)
        assertEquals(
            listOf(
                FormAnswer("target", choice = listOf("prod")),
                FormAnswer("checks", choice = listOf("unit", "e2e")),
                FormAnswer("window", text = "after the freeze"),
                FormAnswer("note", text = "No secret here"),
            ),
            f.answers,
        )
        assertEquals(f, roundTrip(f))
    }

    @Test
    fun theFormMissingLineNamesTheQuestions() {
        val s = sheetOf("request-question-form.json")
        val d = AnswerSheet.empty(s)
        assertEquals("Send is off. Questions 1, 2, 3 and 4 have no valid answer.", AnswerSheet.missingLine(s, d))
        val d2 = AnswerSheet.tap(s, AnswerSheet.tap(s, d, 0, "prod"), 2, "now")
        assertEquals("Send is off. Questions 2 and 4 have no valid answer.", AnswerSheet.missingLine(s, d2))
    }

    // ------------------------------------------------------------ the rules of the core crate

    @Test
    fun theSheetRefusesWhatValidateFormAnswerRefuses() {
        // The cases of `form.rs`: unknown option, wrong count, repeat, text on a choice without allow_custom.
        val s = sheetOf { o ->
            o.put("allow_custom", false)
            o.getJSONObject("input").put("multiple", true).put("min", 1).put("max", 2).put(
                "options",
                JSONArray().put(JSONObject().put("id", "a").put("label", "A")).put(JSONObject().put("id", "b").put("label", "B"))
                    .put(JSONObject().put("id", "c").put("label", "C")),
            )
        }
        fun status(ticks: List<String>) = AnswerSheet.status(s, SheetDraft(listOf(BlockDraft(ticked = ticks))), 0)
        assertEquals(BlockStatus.Complete, status(listOf("a", "c")))
        assertEquals(BlockStatus.Invalid("An option is not valid."), status(listOf("zzz")))
        assertEquals(BlockStatus.Invalid("An option is not valid."), status(listOf("a", "a")))
        assertEquals(BlockStatus.Invalid("Choose at most 2."), status(listOf("a", "b", "c")))
        assertEquals(BlockStatus.Empty, status(emptyList()))
    }
}
