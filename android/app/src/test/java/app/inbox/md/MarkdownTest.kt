package app.inbox.md

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MarkdownTest {
    private fun text(list: List<Inline>): String = list.joinToString("") {
        when (it) {
            is Inline.Text -> it.text
            is Inline.Code -> "`${it.text}`"
            is Inline.Strong -> "**${text(it.children)}**"
            is Inline.Emphasis -> "*${text(it.children)}*"
            is Inline.Strike -> "~~${text(it.children)}~~"
            is Inline.Link -> "[${text(it.children)}](${it.url})"
            Inline.LineBreak -> "\n"
        }
    }

    @Test
    fun headingsParagraphsAndRules() {
        val b = Markdown.parse("# Title\n\nline one\nline two\n\n---\n### Sub ###")
        assertEquals(4, b.size)
        assertEquals(1, (b[0] as Block.Heading).level)
        assertEquals("Title", text((b[0] as Block.Heading).inlines))
        assertEquals("line one\nline two", text((b[1] as Block.Paragraph).inlines))
        assertEquals(Block.Rule, b[2])
        assertEquals("Sub", text((b[3] as Block.Heading).inlines))
    }

    @Test
    fun emphasisCodeAndEscapes() {
        val p = Markdown.parse("**bold** and *it* and ~~gone~~ and `a*b*c` and \\*not\\* and snake_case_name")[0] as Block.Paragraph
        assertEquals("**bold** and *it* and ~~gone~~ and `a*b*c` and *not* and snake_case_name", text(p.inlines))
    }

    @Test
    fun nestedEmphasis() {
        val p = Markdown.parse("**bold *and italic***")[0] as Block.Paragraph
        assertTrue(p.inlines[0] is Inline.Strong)
    }

    @Test
    fun fencedCodeKeepsContentVerbatim() {
        val b = Markdown.parse("before\n```bash\necho **hi** | grep x\n\n  indented\n```\nafter")
        val code = b[1] as Block.CodeBlock
        assertEquals("bash", code.language)
        assertEquals("echo **hi** | grep x\n\n  indented", code.code)
        assertEquals("after", text((b[2] as Block.Paragraph).inlines))
    }

    @Test
    fun unterminatedFenceRunsToTheEnd() {
        val code = Markdown.parse("```\nx\ny")[0] as Block.CodeBlock
        assertEquals("x\ny", code.code)
    }

    @Test
    fun listsWithNesting() {
        val b = Markdown.parse("- a\n- b\n  - b1\n  - b2\n- c\n\n1. one\n2. two")
        val list = b[0] as Block.ListBlock
        assertEquals(false, list.ordered)
        assertEquals(3, list.items.size)
        val nested = list.items[1][1] as Block.ListBlock
        assertEquals(2, nested.items.size)
        val ol = b[1] as Block.ListBlock
        assertEquals(true, ol.ordered)
        assertEquals(1, ol.start)
        assertEquals(2, ol.items.size)
    }

    @Test
    fun orderedListStart() {
        val ol = Markdown.parse("3. three\n4. four")[0] as Block.ListBlock
        assertEquals(3, ol.start)
    }

    @Test
    fun blockQuote() {
        val q = Markdown.parse("> quoted **text**\n> more")[0] as Block.Quote
        assertEquals("quoted **text**\nmore", text((q.blocks[0] as Block.Paragraph).inlines))
    }

    @Test
    fun pipeTables() {
        val t = Markdown.parse("| Host | CPU | State |\n|:-----|----:|:-----:|\n| db-1 | 98% | **down** |\n| `a\\|b` | 3 |")[0] as Block.Table
        assertEquals(listOf(Align.LEFT, Align.RIGHT, Align.CENTER), t.aligns)
        assertEquals("Host", text(t.header[0]))
        assertEquals(2, t.rows.size)
        assertEquals("**down**", text(t.rows[0][2]))
        assertEquals("`a|b`", text(t.rows[1][0]))
        assertEquals("", text(t.rows[1][2]))
    }

    @Test
    fun linksOnlyFollowHttp() {
        val p = Markdown.parse("[ok](https://x.example/a) [bad](javascript:alert(1)) <http://y.example> see https://z.example/p.")[0] as Block.Paragraph
        val links = p.inlines.filterIsInstance<Inline.Link>()
        assertEquals("https://x.example/a", links[0].url)
        assertNull(links[1].url)
        assertEquals("http://y.example", links[2].url)
        assertEquals("https://z.example/p", links[3].url)
    }

    @Test
    fun imagesBecomeLinks() {
        val p = Markdown.parse("![graph](https://g.example/i.png)")[0] as Block.Paragraph
        val l = p.inlines[0] as Inline.Link
        assertEquals("graph", text(l.children))
        assertEquals("https://g.example/i.png", l.url)
    }

    @Test
    fun plainTextAndFirstLine() {
        val md = "## Disk **full** on db-1\n\n| a | b |\n|---|---|\n| 1 | 2 |"
        assertEquals("Disk full on db-1", Markdown.firstLine(md))
        assertTrue(Markdown.plainText(md).contains("1 | 2"))
        assertNull(Markdown.firstLine("   \n\n"))
    }

    @Test
    fun hugeInputIsBounded() {
        val big = "*".repeat(Markdown.MAX_INPUT * 2)
        Markdown.parse(big)
        Markdown.parse("[".repeat(10_000) + "x" + "]".repeat(10_000))
    }
}
