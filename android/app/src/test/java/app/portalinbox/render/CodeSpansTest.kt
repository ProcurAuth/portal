package app.portalinbox.render

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class CodeSpansTest {
    private fun build(lang: String?, text: String, json: String): StyledText =
        CodeSpans.build(lang, text) { _, _ -> json }

    @Test
    fun tokensBecomeSpansAndPlainTokensMakeNone() {
        val s = build("sql", "SELECT 1", """[[0,6,"keyword"],[6,7,"plain"],[7,8,"number"]]""")
        assertEquals("SELECT 1", s.text)
        assertEquals(
            listOf(Span(0, 6, SpanStyle.Token(TokenClass.KEYWORD)), Span(7, 8, SpanStyle.Token(TokenClass.NUMBER))),
            s.spans,
        )
    }

    @Test
    fun theTextIsNeverChanged() {
        val text = "a ‮ b"
        assertEquals(text, build("bash", text, """[[0,1,"keyword"],[1,6,"plain"]]""").text)
    }

    @Test
    fun adjacentTokensOfTheSameClassMerge() {
        val s = build("sql", "abcd", """[[0,2,"string"],[2,4,"string"]]""")
        assertEquals(listOf(Span(0, 4, SpanStyle.Token(TokenClass.STRING))), s.spans)
    }

    @Test
    fun aLanguageOutsideTheClosedListGivesNoColorAndNoCall() {
        var called = false
        val s = CodeSpans.build("cobol", "MOVE 1", CodeSpans.Tokenizer { _, _ -> called = true; "[]" })
        assertTrue(s.spans.isEmpty())
        assertTrue(!called)
        assertTrue(CodeSpans.build(null, "x", CodeSpans.Tokenizer { _, _ -> called = true; "[]" }).spans.isEmpty())
        assertTrue(CodeSpans.build("plain", "x", CodeSpans.Tokenizer { _, _ -> called = true; "[]" }).spans.isEmpty())
        assertTrue(!called)
    }

    @Test
    fun aBadAnswerGivesNoColorAtAll() {
        val text = "SELECT 1"
        for (json in listOf(
            "not json",
            "{}",
            """[[0,3,"keyword"],[2,5,"string"]]""", // overlap
            """[[0,99,"keyword"]]""", // past the end
            """[[3,3,"keyword"]]""", // empty
            """[[0,3,"magic"]]""", // unknown class
            """[[0,3]]""",
            """[["a",3,"keyword"]]""",
            """[[4,6,"keyword"],[0,2,"string"]]""", // out of order
        )) {
            assertTrue(json, build("sql", text, json).spans.isEmpty())
            assertEquals(text, build("sql", text, json).text)
        }
    }

    @Test
    fun aCutInsideASurrogatePairIsRefused() {
        val text = "a😀b" // 4 UTF-16 units
        assertTrue(build("sql", text, """[[0,2,"keyword"]]""").spans.isEmpty())
        assertEquals(2, build("sql", text, """[[0,1,"keyword"],[1,3,"string"]]""").spans.size)
    }

    @Test
    fun aTokenizerThatFailsGivesNoColor() {
        val s = CodeSpans.build("sql", "x", CodeSpans.Tokenizer { _, _ -> throw IllegalStateException("native") })
        assertTrue(s.spans.isEmpty())
    }

    @Test
    fun diffClassesAreKnown() {
        val s = build("diff", "+a\n-b\n", """[[0,3,"add"],[3,6,"del"]]""")
        assertEquals(listOf(TokenClass.ADD, TokenClass.DEL), s.spans.map { (it.style as SpanStyle.Token).cls })
    }
}
