package app.inbox.md

/*
 * Small, dependency-free Markdown parser for alert details (CommonMark / GFM
 * subset): headings, paragraphs, emphasis, strikethrough, inline code, fenced
 * code blocks, bullet and numbered lists (nested), block quotes, links,
 * horizontal rules and pipe tables. Images are kept as links and never loaded.
 *
 * Pure Kotlin (no Android types) so it is unit-tested on the JVM. Nothing is
 * ever executed: the output is a tree rendered to plain Android views.
 */

sealed interface Inline {
    data class Text(val text: String) : Inline
    data class Code(val text: String) : Inline
    data class Strong(val children: List<Inline>) : Inline
    data class Emphasis(val children: List<Inline>) : Inline
    data class Strike(val children: List<Inline>) : Inline
    /** `url` is null when the target is not an http(s) URL: shown as text only. */
    data class Link(val children: List<Inline>, val url: String?) : Inline
    object LineBreak : Inline
}

enum class Align { LEFT, CENTER, RIGHT }

sealed interface Block {
    data class Heading(val level: Int, val inlines: List<Inline>) : Block
    data class Paragraph(val inlines: List<Inline>) : Block
    data class CodeBlock(val language: String?, val code: String) : Block
    data class ListBlock(val ordered: Boolean, val start: Int, val items: List<List<Block>>) : Block
    data class Quote(val blocks: List<Block>) : Block
    object Rule : Block
    data class Table(val aligns: List<Align>, val header: List<List<Inline>>, val rows: List<List<List<Inline>>>) : Block
}

object Markdown {
    /** Upper bound on the input, so a huge message cannot stall the UI thread. */
    const val MAX_INPUT = 64 * 1024
    private const val MAX_DEPTH = 8

    fun parse(text: String): List<Block> =
        parseBlocks(text.take(MAX_INPUT).replace("\r\n", "\n").replace('\r', '\n').replace("\t", "    ").lines(), 0)

    /** Only http and https links are followed; anything else stays plain text. */
    fun safeUrl(raw: String): String? {
        val url = raw.trim().removeSurrounding("<", ">")
        val lower = url.lowercase()
        if (!(lower.startsWith("http://") || lower.startsWith("https://"))) return null
        if (url.any { it.isWhitespace() || it.code < 0x20 }) return null
        return url
    }

    /** Plain text without markup, for notifications and previews. */
    fun plainText(text: String): String = buildString {
        fun inl(list: List<Inline>) {
            for (i in list) when (i) {
                is Inline.Text -> append(i.text)
                is Inline.Code -> append(i.text)
                is Inline.Strong -> inl(i.children)
                is Inline.Emphasis -> inl(i.children)
                is Inline.Strike -> inl(i.children)
                is Inline.Link -> inl(i.children)
                Inline.LineBreak -> append('\n')
            }
        }
        fun blocks(list: List<Block>) {
            for (b in list) {
                when (b) {
                    is Block.Heading -> inl(b.inlines)
                    is Block.Paragraph -> inl(b.inlines)
                    is Block.CodeBlock -> append(b.code)
                    is Block.ListBlock -> b.items.forEach { blocks(it) }
                    is Block.Quote -> blocks(b.blocks)
                    Block.Rule -> {}
                    is Block.Table -> (listOf(b.header) + b.rows).forEach { row ->
                        row.forEachIndexed { k, cell -> if (k > 0) append(" | "); inl(cell) }
                        append('\n')
                    }
                }
                if (isNotEmpty() && last() != '\n') append('\n')
            }
        }
        blocks(parse(text))
    }.trim()

    /** First non-empty line of the plain text. */
    fun firstLine(text: String): String? =
        plainText(text).lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() }

    // ------------------------------------------------------------ blocks

    private val fence = Regex("^ {0,3}(`{3,}|~{3,})\\s*([^`\\s]*)?.*$")
    private val heading = Regex("^ {0,3}(#{1,6})(?:\\s+(.*?))?\\s*#*\\s*$")
    // Java regexes recurse on repeated groups: rules and table delimiters are
    // matched by hand so a very long line cannot overflow the stack.
    private val bullet = Regex("^( *)([-*+])\\s+(.*)$")
    private val ordered = Regex("^( *)(\\d{1,9})[.)]\\s+(.*)$")
    private val quote = Regex("^ {0,3}> ?(.*)$")
    private object rule {
        /** `---`, `***`, `___` (spaces allowed), at most 3 spaces of indent. */
        fun matches(line: String): Boolean {
            if (line.length - line.trimStart(' ').length > 3) return false
            val chars = line.filter { !it.isWhitespace() }
            return chars.length >= 3 && chars[0] in "-*_" && chars.all { it == chars[0] }
        }
    }

    private object tableDelimiter {
        /** `| :--- | ---: | :-: |`: every cell is dashes with optional colons. */
        fun matches(line: String): Boolean {
            val t = line.trim()
            if (t.isEmpty() || !t.contains('-')) return false
            val cells = t.removePrefix("|").removeSuffix("|").split('|')
            return cells.all { c ->
                val x = c.trim()
                val core = x.removePrefix(":").removeSuffix(":")
                core.isNotEmpty() && core.all { it == '-' }
            }
        }
    }

    private fun isBlank(line: String) = line.isBlank()

    private fun startsBlock(line: String, next: String?): Boolean =
        fence.matches(line) || heading.matches(line) || rule.matches(line) || bullet.matches(line) ||
            ordered.matches(line) || quote.matches(line) || (next != null && isTableStart(line, next))

    private fun isTableStart(line: String, next: String): Boolean =
        line.contains('|') && tableDelimiter.matches(next) && splitRow(line).size == splitRow(next).size

    private fun parseBlocks(lines: List<String>, depth: Int): List<Block> {
        val out = mutableListOf<Block>()
        var i = 0
        while (i < lines.size) {
            val line = lines[i]
            val next = lines.getOrNull(i + 1)
            when {
                isBlank(line) -> i++
                fence.matches(line) -> {
                    val m = fence.find(line)!!
                    val marker = m.groupValues[1]
                    val lang = m.groupValues[2].takeIf { it.isNotEmpty() }
                    val body = mutableListOf<String>()
                    i++
                    while (i < lines.size) {
                        val l = lines[i].trimStart()
                        if (l.startsWith(marker) && l.trimEnd().all { it == marker[0] }) {
                            i++
                            break
                        }
                        body.add(lines[i])
                        i++
                    }
                    out.add(Block.CodeBlock(lang, body.joinToString("\n")))
                }
                heading.matches(line) -> {
                    val m = heading.find(line)!!
                    out.add(Block.Heading(m.groupValues[1].length, parseInline(m.groupValues[2], 0)))
                    i++
                }
                rule.matches(line) -> {
                    out.add(Block.Rule)
                    i++
                }
                quote.matches(line) -> {
                    val body = mutableListOf<String>()
                    while (i < lines.size && !isBlank(lines[i])) {
                        val m = quote.find(lines[i])
                        if (m != null) body.add(m.groupValues[1])
                        else if (body.isNotEmpty() && !startsBlock(lines[i], lines.getOrNull(i + 1))) body.add(lines[i])
                        else break
                        i++
                    }
                    out.add(Block.Quote(if (depth < MAX_DEPTH) parseBlocks(body, depth + 1) else listOf(Block.Paragraph(listOf(Inline.Text(body.joinToString(" ")))))))
                }
                bullet.matches(line) || ordered.matches(line) -> {
                    val (block, consumed) = parseList(lines, i, depth)
                    out.add(block)
                    i += consumed
                }
                next != null && isTableStart(line, next) -> {
                    val header = splitRow(line)
                    val aligns = splitRow(next).map {
                        val c = it.trim()
                        when {
                            c.startsWith(":") && c.endsWith(":") -> Align.CENTER
                            c.endsWith(":") -> Align.RIGHT
                            else -> Align.LEFT
                        }
                    }
                    i += 2
                    val rows = mutableListOf<List<List<Inline>>>()
                    while (i < lines.size && !isBlank(lines[i]) && lines[i].contains('|')) {
                        val cells = splitRow(lines[i])
                        rows.add(List(header.size) { k -> parseInline(cells.getOrElse(k) { "" }.trim(), 0) })
                        i++
                    }
                    out.add(Block.Table(aligns, header.map { parseInline(it.trim(), 0) }, rows))
                }
                else -> {
                    val para = mutableListOf<String>()
                    while (i < lines.size && !isBlank(lines[i]) && (para.isEmpty() || !startsBlock(lines[i], lines.getOrNull(i + 1)))) {
                        para.add(lines[i])
                        i++
                    }
                    out.add(Block.Paragraph(paragraphInlines(para)))
                }
            }
        }
        return out
    }

    /**
     * Alert texts are often line-oriented (logs, key: value): unlike strict
     * CommonMark, a single newline inside a paragraph is kept as a line break.
     */
    private fun paragraphInlines(lines: List<String>): List<Inline> {
        val out = mutableListOf<Inline>()
        lines.forEachIndexed { k, raw ->
            if (k > 0) out.add(Inline.LineBreak)
            val l = raw.trim().removeSuffix("\\").trimEnd()
            out.addAll(parseInline(l, 0))
        }
        return out
    }

    private fun parseList(lines: List<String>, from: Int, depth: Int): Pair<Block, Int> {
        val first = bullet.find(lines[from])
        val isOrdered = first == null
        val m0 = first ?: ordered.find(lines[from])!!
        val baseIndent = m0.groupValues[1].length
        val start = if (isOrdered) m0.groupValues[2].toIntOrNull() ?: 1 else 1
        val items = mutableListOf<List<Block>>()
        var i = from
        while (i < lines.size) {
            val line = lines[i]
            val m = (if (isOrdered) ordered else bullet).find(line)
            if (m == null || m.groupValues[1].length != baseIndent) break
            // Content column: the item's continuation lines are indented to it.
            val contentCol = line.length - m.groupValues[3].length
            val body = mutableListOf(m.groupValues[3])
            i++
            var blankRun = 0
            while (i < lines.size) {
                val l = lines[i]
                if (isBlank(l)) {
                    blankRun++
                    i++
                    continue
                }
                val indent = l.length - l.trimStart().length
                val sibling = (if (isOrdered) ordered else bullet).find(l)?.let { it.groupValues[1].length == baseIndent } == true
                if (sibling || indent <= baseIndent && (blankRun > 0 || startsBlock(l, lines.getOrNull(i + 1)))) break
                if (blankRun > 0) repeat(blankRun) { body.add("") }
                blankRun = 0
                body.add(if (indent >= contentCol) l.substring(contentCol) else l.trimStart())
                i++
            }
            items.add(if (depth < MAX_DEPTH) parseBlocks(body, depth + 1) else listOf(Block.Paragraph(listOf(Inline.Text(body.joinToString(" "))))))
        }
        return Block.ListBlock(isOrdered, start, items) to (i - from)
    }

    /** Cells of a pipe-table row; escaped pipes `\|` and pipes in code spans stay in the cell. */
    private fun splitRow(line: String): List<String> {
        var s = line.trim()
        if (s.startsWith("|")) s = s.substring(1)
        if (s.endsWith("|") && !s.endsWith("\\|")) s = s.dropLast(1)
        val cells = mutableListOf<String>()
        val cur = StringBuilder()
        var inCode = false
        var k = 0
        while (k < s.length) {
            val c = s[k]
            when {
                c == '\\' && k + 1 < s.length && s[k + 1] == '|' -> { cur.append('|'); k++ }
                c == '`' -> { inCode = !inCode; cur.append(c) }
                c == '|' && !inCode -> { cells.add(cur.toString()); cur.clear() }
                else -> cur.append(c)
            }
            k++
        }
        cells.add(cur.toString())
        return cells
    }

    // ------------------------------------------------------------ inlines

    private const val PUNCT = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"
    private val bareUrl = Regex("https?://[^\\s<>()\\[\\]]+[^\\s<>()\\[\\].,;:!?'\"]")

    fun parseInline(s: String, depth: Int): List<Inline> {
        val out = mutableListOf<Inline>()
        val text = StringBuilder()
        fun flush() {
            if (text.isNotEmpty()) {
                out.add(Inline.Text(text.toString()))
                text.clear()
            }
        }
        var i = 0
        while (i < s.length) {
            val c = s[i]
            // Escapes.
            if (c == '\\' && i + 1 < s.length && s[i + 1] in PUNCT) {
                text.append(s[i + 1])
                i += 2
                continue
            }
            // Code span: a run of N backticks closed by the same run.
            if (c == '`') {
                var n = 0
                while (i + n < s.length && s[i + n] == '`') n++
                val close = s.indexOf("`".repeat(n), i + n).takeIf { it >= 0 && (it + n >= s.length || s[it + n] != '`') }
                if (close != null) {
                    flush()
                    val code = s.substring(i + n, close)
                    out.add(Inline.Code(if (code.length > 2 && code.startsWith(" ") && code.endsWith(" ")) code.substring(1, code.length - 1) else code))
                    i = close + n
                    continue
                }
                text.append("`".repeat(n))
                i += n
                continue
            }
            // Links and images: [text](url), ![alt](url).
            if ((c == '[' || (c == '!' && i + 1 < s.length && s[i + 1] == '[')) && depth < MAX_DEPTH) {
                val open = if (c == '!') i + 1 else i
                val close = matchingBracket(s, open)
                if (close > 0 && close + 1 < s.length && s[close + 1] == '(') {
                    val end = s.indexOf(')', close + 2)
                    if (end > 0) {
                        flush()
                        val label = s.substring(open + 1, close)
                        val target = s.substring(close + 2, end).trim().split(Regex("\\s+"), limit = 2)[0]
                        val children = if (c == '!') listOf(Inline.Text(label.ifEmpty { "image" })) else parseInline(label, depth + 1)
                        out.add(Inline.Link(children, safeUrl(target)))
                        i = end + 1
                        continue
                    }
                }
            }
            // Autolinks <https://…> and bare URLs.
            if (c == '<') {
                val end = s.indexOf('>', i + 1)
                val url = if (end > 0) safeUrl(s.substring(i + 1, end)) else null
                if (url != null) {
                    flush()
                    out.add(Inline.Link(listOf(Inline.Text(url)), url))
                    i = end + 1
                    continue
                }
            }
            if ((c == 'h' || c == 'H') && (i == 0 || !s[i - 1].isLetterOrDigit())) {
                val m = bareUrl.matchAt(s, i)
                if (m != null) {
                    flush()
                    out.add(Inline.Link(listOf(Inline.Text(m.value)), safeUrl(m.value)))
                    i += m.value.length
                    continue
                }
            }
            // Emphasis: **strong**, __strong__, *em*, _em_, ~~strike~~.
            if ((c == '*' || c == '_' || c == '~') && depth < MAX_DEPTH) {
                val double = i + 1 < s.length && s[i + 1] == c
                if (c == '~' && !double) {
                    text.append(c)
                    i++
                    continue
                }
                val delim = if (double) "$c$c" else "$c"
                val intraword = c == '_' && i > 0 && s[i - 1].isLetterOrDigit()
                val opens = i + delim.length < s.length && !s[i + delim.length].isWhitespace()
                if (!intraword && opens) {
                    val close = findCloser(s, i + delim.length, delim)
                    if (close > 0) {
                        flush()
                        val inner = parseInline(s.substring(i + delim.length, close), depth + 1)
                        out.add(when {
                            c == '~' -> Inline.Strike(inner)
                            double -> Inline.Strong(inner)
                            else -> Inline.Emphasis(inner)
                        })
                        i = close + delim.length
                        continue
                    }
                }
                text.append(delim)
                i += delim.length
                continue
            }
            text.append(c)
            i++
        }
        flush()
        return out
    }

    /** Delimiters are only matched within this distance, so parsing stays linear. */
    private const val SCAN = 4096

    private fun matchingBracket(s: String, open: Int): Int {
        var level = 0
        var k = open
        val end = minOf(s.length, open + SCAN)
        while (k < end) {
            when (s[k]) {
                '\\' -> k++
                '[' -> level++
                ']' -> if (--level == 0) return k
                '`' -> {
                    val end = s.indexOf('`', k + 1)
                    if (end > 0) k = end
                }
            }
            k++
        }
        return -1
    }

    /** Closing delimiter not preceded by whitespace, outside code spans. */
    private fun findCloser(s: String, from: Int, delim: String): Int {
        var k = from
        val end = minOf(s.length, from + SCAN)
        while (k < end) {
            val ch = s[k]
            if (ch == '\\') {
                k += 2
                continue
            }
            if (ch == '`') {
                val end = s.indexOf('`', k + 1)
                if (end > 0) {
                    k = end + 1
                    continue
                }
            }
            if (s.startsWith(delim, k) && k > from && !s[k - 1].isWhitespace()) {
                val after = k + delim.length
                // A single delimiter must not be the start of a double one.
                val tight = delim.length == 1 && after < s.length && s[after] == delim[0]
                val intraword = delim[0] == '_' && after < s.length && s[after].isLetterOrDigit()
                if (!tight && !intraword) return k
                if (tight) {
                    k += 2
                    continue
                }
            }
            k++
        }
        return -1
    }
}
