package app.portalinbox.portal

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AddressDiffTest {
    @Test
    fun equalTextsMarkNothing() {
        assertTrue(AddressDiff.changed("portal.example.org", "portal.example.org").isEmpty())
        assertTrue(AddressDiff.changed("", "x").isEmpty())
    }

    @Test
    fun oneChangedLetterIsMarked() {
        assertEquals(listOf(5..5), AddressDiff.changed("porta1.example.org", "portal.example.org"))
    }

    @Test
    fun anExtraLetterIsMarked() {
        assertEquals(listOf(4..4), AddressDiff.changed("portxal.example.org", "portal.example.org"))
    }

    @Test
    fun aMissingLetterMarksTheLetterAfterTheGap() {
        // "portl" lacks the "a": the "l" after the gap is marked.
        assertEquals(listOf(4..4), AddressDiff.changed("portl.example.org", "portal.example.org"))
        // At the end: the last letter.
        assertEquals(listOf(2..2), AddressDiff.changed("abc", "abcd"))
    }

    @Test
    fun twoChangesGiveTwoRanges() {
        assertEquals(listOf(0..0, 5..5), AddressDiff.changed("qortaX.example.org", "portal.example.org"))
    }

    @Test
    fun neighbourLettersMakeOneRange() {
        assertEquals(listOf(2..3), AddressDiff.changed("abXYef", "abcdef"))
    }

    @Test
    fun rangesStayInsideTheShownText() {
        for ((a, b) in listOf("a" to "zzzzzz", "zzzzzz" to "a", "ab" to "ba", "x.y" to "xy")) {
            for (r in AddressDiff.changed(a, b)) assertTrue("$a $b $r", r.first >= 0 && r.last < a.length)
        }
    }
}
