package app.portalinbox.ui

import app.portalinbox.render.Span
import app.portalinbox.render.SpanStyle
import app.portalinbox.render.StyledText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class VisibleTextTest {
    private fun shown(text: String, nonAscii: Boolean = true) = VisibleText.render(text, nonAscii)

    @Test
    fun plainAsciiTextIsNotChanged() {
        val m = shown("SELECT 1;\n\tFROM t")
        assertEquals("SELECT 1;\n\tFROM t", m.text.text)
        assertTrue(m.text.spans.isEmpty())
        assertFalse(m.hasMarks)
        assertNull(VisibleText.banner(m))
    }

    @Test
    fun aBidiOverrideShowsAsAMark() {
        val m = shown("keep ‮swor wen")
        assertEquals("keep <U+202E>swor wen", m.text.text)
        assertEquals(1, m.hiddenCount)
        assertEquals(listOf(Span(5, 13, SpanStyle.MarkHidden)), m.text.spans)
        assertEquals(VisibleText.Kind.BIDI, m.marks.single().kind)
    }

    @Test
    fun everyBidiControlIsMarked() {
        val all = (0x202A..0x202E) + (0x2066..0x2069) + listOf(0x200E, 0x200F, 0x061C)
        for (cp in all) {
            val m = shown("a${String(Character.toChars(cp))}b")
            assertEquals("U+%04X".format(cp), 1, m.hiddenCount)
            assertEquals(VisibleText.Kind.BIDI, m.marks.single().kind)
            assertEquals("a<U+%04X>b".format(cp), m.text.text)
        }
    }

    @Test
    fun zeroWidthCharactersAreMarked() {
        for (cp in listOf(0x200B, 0x200C, 0x200D, 0x2060, 0xFEFF, 0x00AD, 0x2063, 0x3164, 0x034F)) {
            val m = shown("a${String(Character.toChars(cp))}b")
            assertEquals("U+%04X".format(cp), 1, m.hiddenCount)
            assertEquals(VisibleText.Kind.ZERO_WIDTH, m.marks.single().kind)
        }
    }

    @Test
    fun controlCharactersAreMarkedButNewlineAndTabAreNot() {
        val m = shown("a\u0000b\u001Bc\u007Fd\u0085e\n\tf")
        assertEquals("a<U+0000>b<U+001B>c<U+007F>d<U+0085>e\n\tf", m.text.text)
        assertEquals(4, m.hiddenCount)
    }

    @Test
    fun aWindowsLineEndIsNotAMarkButALoneCarriageReturnIs() {
        assertEquals(0, shown("a\r\nb").hiddenCount)
        assertEquals("a\r\nb", shown("a\r\nb").text.text)
        assertEquals("a<U+000D>b", shown("a\rb").text.text)
    }

    @Test
    fun nonAsciiSpacesAreMarkedAsHidden() {
        val m = shown("a b　c d")
        assertEquals(3, m.hiddenCount)
        assertEquals(VisibleText.Kind.SPACE, m.marks.first().kind)
    }

    @Test
    fun aLoneSurrogateIsMarked() {
        val m = shown("a\uD800b")
        assertEquals("a<U+D800>b", m.text.text)
        assertEquals(1, m.hiddenCount)
    }

    @Test
    fun aCyrillicLetterGetsTheLetterMarkWithItsCode() {
        val m = shown("pаyments")
        assertEquals("pа U+0430yments", m.text.text)
        assertEquals(0, m.hiddenCount)
        assertEquals(1, m.nonAsciiCount)
        assertEquals(listOf(Span(1, 9, SpanStyle.MarkLetter)), m.text.spans)
    }

    @Test
    fun theLetterMarkIsOffForTextsThatCanHoldAccents() {
        val m = shown("café ‮", nonAscii = false)
        assertEquals("café <U+202E>", m.text.text)
        assertEquals(0, m.nonAsciiCount)
        assertEquals(1, m.hiddenCount)
    }

    @Test
    fun aCharacterOutsideTheBasicPlaneCountsOnce() {
        val m = shown("go 😀")
        assertEquals(1, m.nonAsciiCount)
        assertEquals("go 😀 U+1F600", m.text.text)
    }

    @Test
    fun aCombiningMarkShowsItsCodeWithoutAStandAloneLetter() {
        val m = shown("e\u0301")
        assertEquals("eU+0301", m.text.text)
        assertEquals(1, m.nonAsciiCount)
    }

    @Test
    fun theOriginalTextIsNeverChangedByTheRender() {
        val original = "a‮b​c"
        shown(original)
        assertEquals("a‮b​c", original)
    }

    @Test
    fun spansOfTheSourceMoveWithTheText() {
        // "ab" is a keyword, then a zero-width character, then "cd" is a string.
        val source = StyledText(
            "ab​cd",
            listOf(Span(0, 2, SpanStyle.Strong), Span(3, 5, SpanStyle.Emphasis)),
        )
        val m = VisibleText.render(source, nonAscii = false)
        assertEquals("ab<U+200B>cd", m.text.text)
        assertEquals(Span(0, 2, SpanStyle.Strong), m.text.spans[0])
        assertEquals(Span(10, 12, SpanStyle.Emphasis), m.text.spans[1])
        assertEquals(Span(2, 10, SpanStyle.MarkHidden), m.text.spans[2])
    }

    @Test
    fun aSpanThatCoversAMarkedCharacterCoversItsLabel() {
        val source = StyledText("a​b", listOf(Span(0, 3, SpanStyle.Emphasis)))
        val m = VisibleText.render(source, nonAscii = false)
        assertEquals(Span(0, m.text.text.length, SpanStyle.Emphasis), m.text.spans[0])
    }

    @Test
    fun marksAreListedOnceInOrderOfFirstPlace() {
        val m = shown("​ a ‮ b ​ c а")
        assertEquals(listOf(0x200B, 0x202E, 0x0430), m.marks.map { it.codePoint })
        assertEquals(3, m.hiddenCount)
        assertEquals(1, m.nonAsciiCount)
        assertEquals(4, m.markCount)
    }

    @Test
    fun theBannerNamesTheCodesAndIsAnAlarmForHiddenCharacters() {
        val m = shown("pаy ​ ‮")
        assertEquals(
            "Hidden or look-alike characters. 3 marks: U+200B, U+202E (hidden) and U+0430 (not ASCII). Check each mark.",
            VisibleText.banner(m),
        )
        assertTrue(VisibleText.bannerIsAlarm(m))
    }

    @Test
    fun aNonAsciiLetterAloneGivesAMilderBanner() {
        val m = shown("café")
        assertEquals("Characters outside ASCII. 1 mark: U+00E9 (not ASCII). Check each mark.", VisibleText.banner(m))
        assertFalse(VisibleText.bannerIsAlarm(m))
    }

    @Test
    fun theFactsLineSaysAsciiOnlyOrCountsTheMarks() {
        val clean = VisibleText.facts("SELECT 1;\nSELECT 2;", shown("SELECT 1;\nSELECT 2;"), sqlLike = true)
        assertEquals(listOf("2 statements", "2 lines", "19 bytes", "ASCII only"), clean.words())
        val dirty = VisibleText.facts("a​а", shown("a​а"), sqlLike = false)
        assertEquals(listOf("1 line", "6 bytes", "1 hidden", "1 non-ASCII letter"), dirty.words())
    }

    @Test
    fun statementsCountOnlyRealSemicolonsOutsideQuotesAndComments() {
        assertEquals(1, VisibleText.statements("SELECT 1"))
        assertEquals(1, VisibleText.statements("SELECT 1;"))
        assertEquals(2, VisibleText.statements("SELECT 1; SELECT 2"))
        assertEquals(1, VisibleText.statements("SELECT 'a;b'; -- ; not a statement"))
        assertEquals(1, VisibleText.statements("SELECT \"a;b\", `c;d`"))
        assertEquals(1, VisibleText.statements("SELECT 'it''s; ok'"))
        assertEquals(1, VisibleText.statements("/* a; b */ SELECT 1;"))
        assertEquals(0, VisibleText.statements("  \n ; ;"))
        assertEquals(0, VisibleText.statements(""))
    }

    @Test
    fun linesCountTheLastLineWithoutANewline() {
        assertEquals(0, VisibleText.lines(""))
        assertEquals(1, VisibleText.lines("a"))
        assertEquals(1, VisibleText.lines("a\n"))
        assertEquals(2, VisibleText.lines("a\nb"))
    }

    @Test
    fun theMarksOfTheCommandAndOfItsFieldsMakeOneReport() {
        val command = shown("DELETE \u200B x")
        val field = shown("pr\u043Ed")
        val both = VisibleText.combine(listOf(command, field))
        assertEquals(1, both.hiddenCount)
        assertEquals(1, both.nonAsciiCount)
        assertEquals(listOf(0x200B, 0x043E), both.marks.map { it.codePoint })
        assertEquals(
            "Hidden or look-alike characters. 2 marks: U+200B (hidden) and U+043E (not ASCII). Check each mark.",
            VisibleText.banner(both),
        )
    }

    @Test
    fun escapeLineMarksHiddenCharactersOfAShortText() {
        assertEquals("ops<U+202E>agent", VisibleText.escapeLine("ops‮agent"))
        assertEquals("café", VisibleText.escapeLine("café"))
    }
}
