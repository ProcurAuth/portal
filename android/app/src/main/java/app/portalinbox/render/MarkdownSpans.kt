package app.portalinbox.render

import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

/** One block of a shown Markdown text. A run of text blocks is one [Flow]. A code block is its own [Code]. */
sealed interface MdBlock {
    /** Paragraphs, headings, quotes and lists, as one text with spans. */
    data class Flow(val text: StyledText) : MdBlock

    /** A fenced code block. [lang] is `plain` or a name of the closed list. [text] holds the colors. */
    data class Code(val lang: String, val text: StyledText) : MdBlock
}

/** A shown Markdown text. [fallback] is true when a limit made the whole text plain. */
data class MdDocument(val blocks: List<MdBlock>, val fallback: Boolean)

/**
 * Builds what the phone shows for a Markdown explanation: the tree of `inbox_core::render::markdown`
 * (`Native.markdownParse`, JSON text) becomes spans, with no WebView and no HTML.
 *
 * Rules (the safe subset):
 * - a link is never active: it shows its text, then ` (host)`; a link with no host shows ` (no web address)`;
 * - a heading is a bold line (level 1 to 3), an item of a task list has a read-only box;
 * - a list shows its markers as text (`•`, `◦`, `▪` by depth, `1.` for an ordered list);
 * - a code block inside a quote or a list item comes out as a block of its own, after the text before it;
 * - a node of a kind that this version does not know shows nothing, and never fails.
 */
object MarkdownSpans {
    fun interface Parser {
        /** The tree of a Markdown text, as JSON text. The real one is `Native.markdownParse`. */
        fun parse(text: String): String
    }

    const val NO_ADDRESS = "no web address"
    private const val MAX_DEPTH = 8

    fun build(markdown: String, parser: Parser, tokenizer: CodeSpans.Tokenizer): MdDocument {
        val tree = try {
            JSONObject(parser.parse(markdown))
        } catch (e: JSONException) {
            return plain(markdown)
        } catch (e: RuntimeException) {
            return plain(markdown)
        }
        val b = Builder(tokenizer)
        val blocks = tree.optJSONArray("blocks") ?: return plain(markdown)
        b.blocks(blocks, 0)
        return MdDocument(b.finish(), tree.optBoolean("fallback", false))
    }

    private fun plain(text: String) = MdDocument(listOf(MdBlock.Flow(StyledText(text))), true)

    private class Builder(private val tokenizer: CodeSpans.Tokenizer) {
        private val out = ArrayList<MdBlock>()
        private val sb = StringBuilder()
        private val spans = ArrayList<Span>()

        fun finish(): List<MdBlock> {
            flush()
            return out
        }

        private fun flush() {
            if (sb.isEmpty()) return
            out += MdBlock.Flow(StyledText(sb.toString(), ArrayList(spans)))
            sb.setLength(0)
            spans.clear()
        }

        /** Starts a new line, with a small gap when [gap] says a block ends. */
        private fun lineBreak(gap: Boolean) {
            if (sb.isEmpty()) return
            if (sb.last() != '\n') sb.append('\n')
            if (gap && !endsWithGap()) {
                spans += Span(sb.length, sb.length + 1, SpanStyle.Gap)
                sb.append('\n')
            }
        }

        private fun endsWithGap(): Boolean = spans.lastOrNull()?.let { it.style == SpanStyle.Gap && it.end == sb.length } == true

        fun blocks(array: JSONArray, depth: Int, listDepth: Int = 0) {
            if (depth > MAX_DEPTH) return
            for (n in 0 until array.length()) {
                val block = array.optJSONObject(n) ?: continue
                when (block.optString("t")) {
                    "paragraph" -> {
                        lineBreak(gap = true)
                        val start = sb.length
                        inlines(block.optJSONArray("c"), 0)
                        if (listDepth > 0) spans += Span(start, sb.length, SpanStyle.Indent(listDepth, hanging = false))
                    }
                    "heading" -> {
                        lineBreak(gap = true)
                        val start = sb.length
                        inlines(block.optJSONArray("c"), 0)
                        val level = block.optInt("level", 1).coerceIn(1, 3)
                        spans += Span(start, sb.length, SpanStyle.Heading(level))
                    }
                    "code" -> {
                        flush()
                        val text = block.optString("text")
                        val lang = block.optString("lang", "plain")
                        out += MdBlock.Code(lang, CodeSpans.build(lang, text, tokenizer))
                    }
                    "quote" -> {
                        lineBreak(gap = true)
                        val start = sb.length
                        val before = out.size
                        blocks(block.optJSONArray("c") ?: JSONArray(), depth + 1, listDepth = listDepth)
                        // A code block inside the quote flushed the text: the bar covers the text that is left.
                        if (out.size == before && sb.length > start) spans += Span(start, sb.length, SpanStyle.Quote)
                    }
                    "list" -> list(block, depth, listDepth + 1)
                    "text" -> {
                        lineBreak(gap = true)
                        sb.append(block.optString("s"))
                    }
                    else -> {}
                }
            }
        }

        private fun list(list: JSONObject, depth: Int, listDepth: Int) {
            val items = list.optJSONArray("items") ?: return
            val ordered = list.optBoolean("ordered", false)
            var number = list.optLong("start", 1)
            lineBreak(gap = listDepth == 1)
            for (n in 0 until items.length()) {
                val item = items.optJSONObject(n) ?: continue
                lineBreak(gap = false)
                val start = sb.length
                val checked = if (item.has("checked")) item.optBoolean("checked") else null
                if (checked != null) {
                    val boxStart = sb.length
                    sb.append(if (checked) "☑" else "☐")
                    spans += Span(boxStart, sb.length, SpanStyle.TaskBox)
                    sb.append(' ')
                } else if (ordered) {
                    sb.append(number).append(". ")
                } else {
                    sb.append(BULLETS[(listDepth - 1).coerceIn(0, BULLETS.size - 1)]).append(' ')
                }
                number++
                val content = item.optJSONArray("c") ?: JSONArray()
                var first = true
                for (k in 0 until content.length()) {
                    val child = content.optJSONObject(k) ?: continue
                    if (first && child.optString("t") == "paragraph") {
                        inlines(child.optJSONArray("c"), 0)
                        spans += Span(start, sb.length, SpanStyle.Indent(listDepth, hanging = true))
                    } else {
                        if (first) spans += Span(start, sb.length, SpanStyle.Indent(listDepth, hanging = true))
                        val one = JSONArray().put(child)
                        blocks(one, depth + 1, listDepth = listDepth)
                    }
                    first = false
                }
                if (first) spans += Span(start, sb.length, SpanStyle.Indent(listDepth, hanging = true))
            }
        }

        private fun inlines(array: JSONArray?, depth: Int) {
            if (array == null || depth > MAX_DEPTH) return
            for (n in 0 until array.length()) {
                val node = array.optJSONObject(n) ?: continue
                when (node.optString("t")) {
                    "text" -> sb.append(node.optString("s"))
                    "code" -> styled(SpanStyle.InlineCode) { sb.append(node.optString("s")) }
                    "em" -> styled(SpanStyle.Emphasis) { inlines(node.optJSONArray("c"), depth + 1) }
                    "strong" -> styled(SpanStyle.Strong) { inlines(node.optJSONArray("c"), depth + 1) }
                    "link" -> {
                        inlines(node.optJSONArray("c"), depth + 1)
                        val host = node.optString("host").ifEmpty { NO_ADDRESS }
                        styled(SpanStyle.LinkHost) { sb.append(" (").append(host).append(')') }
                    }
                    "br" -> sb.append('\n')
                    "soft" -> sb.append(' ')
                    else -> {}
                }
            }
        }

        private inline fun styled(style: SpanStyle, body: () -> Unit) {
            val start = sb.length
            body()
            if (sb.length > start) spans += Span(start, sb.length, style)
        }
    }

    private val BULLETS = charArrayOf('•', '◦', '▪')
}
