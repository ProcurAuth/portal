package app.portalinbox.data

import app.portalinbox.proto.Action
import app.portalinbox.testsupport.Make
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** DIAL 1.4.2: with `multiple` and `allow_custom`, the free answer excludes the ticks, and `min` and `max` count the ticks only. */
class MultipleWithCustomExclusiveTest {
    private fun opt(id: String) = JSONObject().put("id", id).put("label", id.uppercase())

    private fun sheet(custom: Boolean = true, min: Int = 1, max: Int = 2): Sheet {
        val f = Make.question(Make.hex(0xb), Make.hex(1)) { o ->
            o.put("allow_custom", custom)
            o.getJSONObject("input").put("multiple", true).put("min", min).put("max", max)
                .put("options", JSONArray().put(opt("a")).put(opt("b")).put(opt("c")))
        }
        return AnswerSheet.of(f)!!
    }

    private fun tick(s: Sheet, d: SheetDraft, vararg ids: String) = ids.fold(d) { acc, id -> AnswerSheet.tap(s, acc, 0, id) }

    @Test
    fun aTickClearsTheFreeAnswer() {
        val s = sheet()
        var d = AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, "my own")
        assertTrue(d.blocks[0].otherOn)
        d = AnswerSheet.tap(s, d, 0, "a")
        assertFalse(d.blocks[0].otherOn)
        assertEquals(listOf("a"), d.blocks[0].ticked)
    }

    @Test
    fun theFreeAnswerClearsTheTicks() {
        val s = sheet()
        var d = tick(s, AnswerSheet.empty(s), "a", "b")
        d = AnswerSheet.tapOther(s, d, 0)
        assertTrue(d.blocks[0].otherOn)
        assertTrue(d.blocks[0].ticked.isEmpty())
        // Writing in the field does the same: it selects the row.
        d = tick(s, AnswerSheet.empty(s), "c")
        d = AnswerSheet.typeOther(s, d, 0, "x")
        assertTrue(d.blocks[0].otherOn)
        assertTrue(d.blocks[0].ticked.isEmpty())
    }

    @Test
    fun aSecondTapOnTheOtherRowOfACheckboxTurnsItOffAndTheTextStaysInTheField() {
        val s = sheet()
        var d = AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, "keep me")
        d = AnswerSheet.tapOther(s, d, 0)
        assertFalse(d.blocks[0].otherOn)
        assertEquals("keep me", d.blocks[0].otherText)
        assertEquals(BlockStatus.Empty, AnswerSheet.status(s, d, 0))
    }

    @Test
    fun minAndMaxCountTheTicksOnly() {
        val s = sheet(min = 2, max = 2)
        // One tick is below min.
        var d = tick(s, AnswerSheet.empty(s), "a")
        assertEquals(BlockStatus.Invalid("Choose at least 2."), AnswerSheet.status(s, d, 0))
        d = tick(s, d, "b")
        assertEquals(BlockStatus.Complete, AnswerSheet.status(s, d, 0))
        // A third tick past max does not change the draft.
        assertTrue(AnswerSheet.atMax(s, d, 0, "c"))
        assertEquals(d, AnswerSheet.tap(s, d, 0, "c"))
        // The free answer is one text and needs no tick, whatever min says.
        val free = AnswerSheet.typeOther(s, AnswerSheet.empty(s), 0, "just this")
        assertEquals(BlockStatus.Complete, AnswerSheet.status(s, free, 0))
    }

    @Test
    fun theFrameCarriesTheTextAndNoChoiceWhenTheFreeAnswerIsOn() {
        val s = sheet()
        val d = AnswerSheet.typeOther(s, tick(s, AnswerSheet.empty(s), "a"), 0, "my own")
        val built = AnswerSheet.build(s, d, Make.hex(0xb), "m-1") as Built.Frame
        assertNull(built.frame.choice)
        assertEquals("my own", built.frame.text)
        assertEquals(Action.ACCEPT, built.frame.action)
    }

    @Test
    fun aDraftWithTicksAndTheFreeAnswerBothSetIsNotValid() {
        // The functions of the sheet never make it, but a draft that came from elsewhere must not pass.
        val s = sheet()
        val bad = SheetDraft(listOf(BlockDraft(ticked = listOf("a"), otherOn = true, otherText = "x")))
        assertEquals(BlockStatus.Invalid("A free answer excludes the options."), AnswerSheet.status(s, bad, 0))
        assertTrue(AnswerSheet.build(s, bad, Make.hex(0xb), "m-1") is Built.Refused)
    }

    @Test
    fun withoutAllowCustomThereIsNoFreeAnswerAtAll() {
        val s = sheet(custom = false)
        assertFalse(s.blocks[0].otherAllowed)
        val d = AnswerSheet.empty(s)
        assertEquals(d, AnswerSheet.tapOther(s, d, 0))
        assertEquals(d, AnswerSheet.typeOther(s, d, 0, "x"))
        // A draft that has the free answer anyway counts as ticks: it is empty, so not complete.
        val forced = SheetDraft(listOf(BlockDraft(otherOn = true, otherText = "x")))
        assertEquals(BlockStatus.Empty, AnswerSheet.status(s, forced, 0))
    }

    @Test
    fun aRadioNeverHoldsTwoTicksAndTheFreeAnswerReplacesTheTick() {
        val f = Make.question(Make.hex(0xb), Make.hex(1))
        val s = AnswerSheet.of(f)!!
        var d = AnswerSheet.tap(s, AnswerSheet.empty(s), 0, "go")
        d = AnswerSheet.tap(s, d, 0, "wait")
        assertEquals(listOf("wait"), d.blocks[0].ticked)
        d = AnswerSheet.tapOther(s, d, 0)
        assertTrue(d.blocks[0].ticked.isEmpty())
        // A second tap on the other row of a radio keeps it on.
        d = AnswerSheet.tapOther(s, d, 0)
        assertTrue(d.blocks[0].otherOn)
        d = AnswerSheet.tap(s, d, 0, "go")
        assertFalse(d.blocks[0].otherOn)
    }
}
