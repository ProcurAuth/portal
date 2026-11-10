package app.portalinbox.data

import app.portalinbox.proto.InputType
import app.portalinbox.proto.Interaction
import app.portalinbox.testsupport.Make
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** DIAL 1.4.2 and 1.6: the rows under the options come from the two flags, for every combination and every type of input. */
class FlagsMatrixRowsTest {
    private val conv = Interaction.CONVERSATIONAL
    private val shot = Interaction.ONE_SHOT

    private fun rows(type: InputType, custom: Boolean, ask: Boolean, who: Interaction = conv) =
        FlagsMatrix.rows(type, custom, ask, who)

    @Test
    fun aChoiceFollowsTheFourRowsOfTheMatrix() {
        for (type in listOf(InputType.CHOICE, InputType.CONFIRM)) {
            val closed = rows(type, false, false)
            assertFalse(closed.otherAnswer)
            assertFalse(closed.askAgent)
            assertTrue(closed.closed)
            assertNull(closed.invalid)
            assertEquals(FlagsMatrix.Rows(true, false, null), rows(type, true, false))
            assertEquals(FlagsMatrix.Rows(false, true, null), rows(type, false, true))
            assertEquals(FlagsMatrix.Rows(true, true, null), rows(type, true, true))
        }
    }

    @Test
    fun aTextQuestionHasNoOtherRowBecauseItsFieldIsTheAnswer() {
        assertEquals(FlagsMatrix.Rows(false, false, null), rows(InputType.TEXT, false, false))
        assertEquals(FlagsMatrix.Rows(false, true, null), rows(InputType.TEXT, false, true))
        for (ask in listOf(false, true)) {
            val r = rows(InputType.TEXT, true, ask)
            assertEquals(FlagsMatrix.Invalid.TEXT_WITH_CUSTOM, r.invalid)
            assertFalse(r.otherAnswer)
            assertFalse(r.askAgent)
        }
    }

    @Test
    fun aMessageWithNoAnswerTakesNoFlag() {
        assertTrue(rows(InputType.NONE, false, false).closed)
        for ((c, a) in listOf(true to false, false to true, true to true)) {
            val r = rows(InputType.NONE, c, a)
            assertEquals(FlagsMatrix.Invalid.NONE_WITH_FLAG, r.invalid)
            assertTrue(r.closed)
        }
    }

    @Test
    fun aFormHasTheOtherRowOnlyWhenItHasAChoice() {
        assertEquals(FlagsMatrix.Rows(true, true, null), FlagsMatrix.rows(InputType.FORM, true, true, conv, formHasChoice = true))
        assertEquals(FlagsMatrix.Rows(false, true, null), FlagsMatrix.rows(InputType.FORM, false, true, conv, formHasChoice = false))
        val bad = FlagsMatrix.rows(InputType.FORM, true, false, conv, formHasChoice = false)
        assertEquals(FlagsMatrix.Invalid.FORM_CUSTOM_WITHOUT_CHOICE, bad.invalid)
        assertTrue(bad.closed)
    }

    @Test
    fun anAutomaticSystemHasNoRowForAnyFlagAndAnyType() {
        for (type in InputType.entries) {
            for (c in listOf(false, true)) {
                for (a in listOf(false, true)) {
                    val r = rows(type, c, a, shot)
                    assertTrue("$type $c $a", r.closed)
                    val expected = when {
                        c || a -> FlagsMatrix.Invalid.ONE_SHOT_WITH_FLAG
                        type == InputType.CHOICE || type == InputType.CONFIRM -> null
                        else -> FlagsMatrix.Invalid.ONE_SHOT_INPUT
                    }
                    assertEquals("$type $c $a", expected, r.invalid)
                }
            }
        }
    }

    @Test
    fun everyCombinationGivesAnAnswerAndAnInvalidOneNeverShowsARow() {
        var valid = 0
        for (type in InputType.entries) for (c in listOf(false, true)) for (a in listOf(false, true)) for (who in Interaction.entries) {
            val r = FlagsMatrix.rows(type, c, a, who, formHasChoice = true)
            if (r.invalid != null) {
                assertTrue("an invalid combination has no escape row: $type $c $a $who", r.closed)
            } else {
                valid++
                // A row shows only when the agent asked for it.
                assertTrue(!r.otherAnswer || c)
                assertTrue(!r.askAgent || a)
                assertTrue(!r.askAgent || who == conv)
            }
        }
        // 5 types x 4 flag pairs x 2 kinds of sender = 40 combinations, 17 of them valid.
        assertEquals(17, valid)
    }

    // ------------------------------------------------------------ from a frame

    private fun frame(file: String, edit: (JSONObject) -> Unit = {}) = Make.question(Make.hex(0xb), Make.hex(1), file = file, edit = edit)

    @Test
    fun aFrameGivesTheSameRowsAsItsFlags() {
        // The golden question has both flags.
        assertEquals(FlagsMatrix.Rows(true, true, null), FlagsMatrix.rows(frame("request-question-choice.json")))
        val closed = frame("request-question-choice.json") { it.put("allow_custom", false).put("allow_ask_back", false) }
        assertTrue(FlagsMatrix.rows(closed).closed)
        val onlyCustom = frame("request-question-choice.json") { it.put("allow_ask_back", false) }
        assertEquals(FlagsMatrix.Rows(true, false, null), FlagsMatrix.rows(onlyCustom))
        // A text question with the flag ask only.
        assertEquals(FlagsMatrix.Rows(false, true, null), FlagsMatrix.rows(frame("request-question-text-until.json")))
        // The automatic confirm of the golden files is closed.
        assertTrue(FlagsMatrix.rows(frame("request-question-confirm-one-shot.json")).closed)
    }

    @Test
    fun aFormWithAFlagHasTheOtherRowInItsChoiceQuestions() {
        val f = frame("request-question-form.json")
        assertEquals(FlagsMatrix.Rows(true, true, null), FlagsMatrix.rows(f))
        val sheet = AnswerSheet.of(f)!!
        assertEquals(listOf(true, true, true, false), sheet.blocks.map { it.otherAllowed })
    }

    @Test
    fun anApprovalHasNoQuestionRows() {
        val a = Make.approval(Make.hex(0xc), Make.hex(2))
        assertTrue(FlagsMatrix.rows(a).closed)
        assertNull(AnswerSheet.of(a))
    }

    @Test
    fun theSheetOffersOtherOnlyWhenTheMatrixDoes() {
        val closed = frame("request-question-choice.json") { it.put("allow_custom", false) }
        assertFalse(AnswerSheet.of(closed)!!.blocks[0].otherAllowed)
        val open = frame("request-question-choice.json") { it.put("allow_custom", true) }
        assertTrue(AnswerSheet.of(open)!!.blocks[0].otherAllowed)
        // A list of options for a multiple choice keeps the other row too.
        val many = frame("request-question-choice.json") { o ->
            val input = o.getJSONObject("input").put("multiple", true).put("min", 1).put("max", 2)
            input.put("options", JSONArray().put(opt("a")).put(opt("b")).put(opt("c")))
        }
        assertTrue(AnswerSheet.of(many)!!.blocks[0].otherAllowed)
    }

    private fun opt(id: String) = JSONObject().put("id", id).put("label", id.uppercase())
}
