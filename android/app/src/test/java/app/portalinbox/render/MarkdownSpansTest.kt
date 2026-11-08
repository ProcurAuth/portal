package app.portalinbox.render

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MarkdownSpansTest {
    private val noColor = CodeSpans.Tokenizer { _, _ -> "[]" }

    private fun build(tree: String): MdDocument = MarkdownSpans.build("source", { tree }, noColor)

    private fun flow(d: MdDocument, n: Int = 0): StyledText = (d.blocks[n] as MdBlock.Flow).text

    @Test
    fun aParagraphWithEmphasisAndStrongAndCode() {
        val d = build(
            """{"blocks":[{"t":"paragraph","c":[{"t":"text","s":"a "},{"t":"em","c":[{"t":"text","s":"b"}]},{"t":"text","s":" "},
            {"t":"strong","c":[{"t":"text","s":"c"}]},{"t":"soft"},{"t":"code","s":"d()"}]}]}""".replace("\n", ""),
        )
        val t = flow(d)
        assertEquals("a b c d()", t.text)
        assertEquals(
            listOf(Span(2, 3, SpanStyle.Emphasis), Span(4, 5, SpanStyle.Strong), Span(6, 9, SpanStyle.InlineCode)),
            t.spans,
        )
        assertFalse(d.fallback)
    }

    @Test
    fun aLinkIsNeverActiveItShowsTextAndHost() {
        val t = flow(build("""{"blocks":[{"t":"paragraph","c":[{"t":"link","c":[{"t":"text","s":"runbook"}],"host":"example.org"}]}]}"""))
        assertEquals("runbook (example.org)", t.text)
        assertEquals(listOf(Span(7, 21, SpanStyle.LinkHost)), t.spans)
        assertTrue(t.spans.none { it.style !is SpanStyle.LinkHost })
    }

    @Test
    fun aLinkWithNoHostSaysNoWebAddress() {
        val t = flow(build("""{"blocks":[{"t":"paragraph","c":[{"t":"link","c":[{"t":"text","s":"x"}],"host":""}]}]}"""))
        assertEquals("x (no web address)", t.text)
    }

    @Test
    fun headingsAreBoldLinesOfLevelOneToThree() {
        val d = build(
            """{"blocks":[{"t":"heading","level":1,"c":[{"t":"text","s":"A"}]},{"t":"heading","level":3,"c":[{"t":"text","s":"B"}]},
            {"t":"heading","level":9,"c":[{"t":"text","s":"C"}]}]}""".replace("\n", ""),
        )
        val heads = flow(d).spans.filter { it.style is SpanStyle.Heading }.map { (it.style as SpanStyle.Heading).level }
        assertEquals(listOf(1, 3, 3), heads)
        assertTrue(flow(d).text.startsWith("A\n"))
    }

    @Test
    fun blocksAreSeparatedByALineBreakAndASmallGap() {
        val t = flow(build("""{"blocks":[{"t":"paragraph","c":[{"t":"text","s":"one"}]},{"t":"paragraph","c":[{"t":"text","s":"two"}]}]}"""))
        assertEquals("one\n\ntwo", t.text)
        assertEquals(listOf(Span(4, 5, SpanStyle.Gap)), t.spans)
    }

    @Test
    fun anUnorderedListNestsWithThreeBullets() {
        val tree = """{"blocks":[{"t":"list","ordered":false,"start":1,"items":[
            {"c":[{"t":"paragraph","c":[{"t":"text","s":"top"}]},{"t":"list","ordered":false,"start":1,"items":[
              {"c":[{"t":"paragraph","c":[{"t":"text","s":"mid"}]},{"t":"list","ordered":false,"start":1,"items":[
                {"c":[{"t":"paragraph","c":[{"t":"text","s":"deep"}]}]}]}]}]}]}]}]}""".replace("\n", "")
        val t = flow(build(tree))
        assertEquals("• top\n◦ mid\n▪ deep", t.text)
        val indents = t.spans.filter { it.style is SpanStyle.Indent }.map { (it.style as SpanStyle.Indent).level }
        assertEquals(listOf(1, 2, 3), indents)
    }

    @Test
    fun anOrderedListStartsAtItsNumber() {
        val t = flow(
            build(
                """{"blocks":[{"t":"list","ordered":true,"start":3,"items":[{"c":[{"t":"paragraph","c":[{"t":"text","s":"a"}]}]},
                {"c":[{"t":"paragraph","c":[{"t":"text","s":"b"}]}]}]}]}""".replace("\n", ""),
            ),
        )
        assertEquals("3. a\n4. b", t.text)
    }

    @Test
    fun aTaskItemHasAReadOnlyBox() {
        val t = flow(
            build(
                """{"blocks":[{"t":"list","ordered":false,"start":1,"items":[{"checked":true,"c":[{"t":"paragraph","c":[{"t":"text","s":"done"}]}]},
                {"checked":false,"c":[{"t":"paragraph","c":[{"t":"text","s":"todo"}]}]}]}]}""".replace("\n", ""),
            ),
        )
        assertEquals("☑ done\n☐ todo", t.text)
        assertEquals(2, t.spans.count { it.style == SpanStyle.TaskBox })
    }

    @Test
    fun aQuoteGetsABar() {
        val t = flow(build("""{"blocks":[{"t":"quote","c":[{"t":"paragraph","c":[{"t":"text","s":"I do not drop"}]}]}]}"""))
        assertEquals("I do not drop", t.text)
        assertEquals(listOf(Span(0, 13, SpanStyle.Quote)), t.spans)
    }

    @Test
    fun aCodeBlockIsABlockOfItsOwnWithColors() {
        val d = MarkdownSpans.build(
            "s",
            { """{"blocks":[{"t":"paragraph","c":[{"t":"text","s":"before"}]},{"t":"code","lang":"sql","text":"select 1"},{"t":"paragraph","c":[{"t":"text","s":"after"}]}]}""" },
            { lang, text -> if (lang == "sql" && text == "select 1") """[[0,6,"keyword"]]""" else "[]" },
        )
        assertEquals(3, d.blocks.size)
        assertEquals("before", flow(d, 0).text)
        val code = d.blocks[1] as MdBlock.Code
        assertEquals("sql", code.lang)
        assertEquals("select 1", code.text.text)
        assertEquals(listOf(Span(0, 6, SpanStyle.Token(TokenClass.KEYWORD))), code.text.spans)
        assertEquals("after", flow(d, 2).text)
    }

    @Test
    fun aCodeBlockInAListItemComesOutAsItsOwnBlock() {
        val d = build(
            """{"blocks":[{"t":"list","ordered":false,"start":1,"items":[{"c":[{"t":"paragraph","c":[{"t":"text","s":"run"}]},
            {"t":"code","lang":"bash","text":"ls"}]}]}]}""".replace("\n", ""),
        )
        assertEquals(2, d.blocks.size)
        assertEquals("• run", flow(d).text)
        assertEquals("ls", (d.blocks[1] as MdBlock.Code).text.text)
    }

    @Test
    fun theFallbackFlagCarriesOverAndThePlainBlockShows() {
        val d = build("""{"fallback":true,"blocks":[{"t":"text","s":"# raw\n<b>x</b>"}]}""")
        assertTrue(d.fallback)
        assertEquals("# raw\n<b>x</b>", flow(d).text)
    }

    @Test
    fun rawHtmlAndImageSyntaxShowAsPlainText() {
        val t = flow(build("""{"blocks":[{"t":"paragraph","c":[{"t":"text","s":"<img src=x onerror=1> ![a](b)"}]}]}"""))
        assertEquals("<img src=x onerror=1> ![a](b)", t.text)
        assertTrue(t.spans.isEmpty())
    }

    @Test
    fun anUnknownNodeShowsNothingAndNeverFails() {
        val d = build("""{"blocks":[{"t":"table","rows":[]},{"t":"paragraph","c":[{"t":"blink","s":"x"},{"t":"text","s":"ok"}]}]}""")
        assertEquals("ok", flow(d).text)
    }

    @Test
    fun aBrokenTreeGivesThePlainSource() {
        val d = MarkdownSpans.build("**x**", { "not json" }, noColor)
        assertTrue(d.fallback)
        assertEquals("**x**", flow(d).text)
        val d2 = MarkdownSpans.build("src", { throw IllegalStateException("native") }, noColor)
        assertEquals("src", flow(d2).text)
    }

    @Test
    fun aHardBreakIsALineBreak() {
        val t = flow(build("""{"blocks":[{"t":"paragraph","c":[{"t":"text","s":"a"},{"t":"br"},{"t":"text","s":"b"}]}]}"""))
        assertEquals("a\nb", t.text)
    }

    @Test
    fun theHiddenCharactersOfAnExplanationStayVisibleAfterTheRender() {
        val t = flow(build("""{"blocks":[{"t":"paragraph","c":[{"t":"text","s":"Safe to approve‮"}]}]}"""))
        val marked = app.portalinbox.ui.VisibleText.render(t, nonAscii = false)
        assertEquals("Safe to approve<U+202E>", marked.text.text)
    }
}
