package app.portalinbox.ui

import app.portalinbox.render.Span
import app.portalinbox.render.SpanStyle
import app.portalinbox.render.StyledText

/**
 * Makes the characters that the eye cannot see visible. Pure Kotlin: no Context.
 *
 * Nothing is removed and nothing is refused. A control, bidi, zero-width or space character of the
 * range that follows is replaced on the screen by a mark like `<U+202E>`. The text that the owner
 * copies is the original text: the copy never goes through this class.
 *
 * A letter or a symbol outside ASCII can look like an ASCII one (a Cyrillic "a"). For the command, the
 * screen also marks it with the letter and its code. For other texts this mark is off: an explanation
 * can hold honest accents, and a chip on each of them would hide the hidden characters.
 */
object VisibleText {
    enum class Kind(val hidden: Boolean) {
        CONTROL(true), BIDI(true), ZERO_WIDTH(true), SPACE(true), NON_ASCII(false)
    }

    /** One mark that the screen shows: the code point, its kind and the text that stands for it. */
    data class Mark(val codePoint: Int, val kind: Kind) {
        val label: String get() = labelOf(codePoint, kind)
        val code: String get() = codeOf(codePoint)
    }

    fun codeOf(cp: Int): String = "U+%04X".format(cp)

    private fun labelOf(cp: Int, kind: Kind): String {
        if (kind != Kind.NON_ASCII) return "<${codeOf(cp)}>"
        val t = Character.getType(cp)
        val combining = t == Character.NON_SPACING_MARK.toInt() || t == Character.ENCLOSING_MARK.toInt() ||
            t == Character.COMBINING_SPACING_MARK.toInt()
        val letter = if (combining) "" else String(Character.toChars(cp)) + " "
        return letter + codeOf(cp)
    }

    private val BIDI = setOf(0x061C, 0x200E, 0x200F)
    private val FILLERS = setOf(0x034F, 0x115F, 0x1160, 0x2800, 0x3164, 0xFFA0)

    /**
     * The kind of a character, or null when the screen shows it as it is. [next] is the code point after
     * it: a `\r` before `\n` is the end of a Windows line and is not a mark.
     */
    fun kindOf(cp: Int, next: Int, nonAscii: Boolean): Kind? {
        if (cp < 0x80) {
            return when {
                cp == '\n'.code || cp == '\t'.code -> null
                cp == '\r'.code && next == '\n'.code -> null
                cp < 0x20 || cp == 0x7F -> Kind.CONTROL
                else -> null
            }
        }
        if (cp in 0x202A..0x202E || cp in 0x2066..0x2069 || cp in BIDI) return Kind.BIDI
        return when (Character.getType(cp)) {
            Character.CONTROL.toInt(), Character.SURROGATE.toInt() -> Kind.CONTROL
            Character.FORMAT.toInt() -> Kind.ZERO_WIDTH
            Character.SPACE_SEPARATOR.toInt(), Character.LINE_SEPARATOR.toInt(), Character.PARAGRAPH_SEPARATOR.toInt() -> Kind.SPACE
            else -> when {
                cp in FILLERS || cp in 0xFE00..0xFE0F || cp in 0xE0100..0xE01EF -> Kind.ZERO_WIDTH
                nonAscii -> Kind.NON_ASCII
                else -> null
            }
        }
    }

    /** What the screen shows for a text: the text with the marks in place, and the facts about the marks. */
    data class Marked(
        val text: StyledText,
        /** Each distinct mark, in the order of its first place. */
        val marks: List<Mark>,
        /** Number of hidden characters (control, bidi, zero-width, space). */
        val hiddenCount: Int,
        /** Number of characters outside ASCII that got the letter mark. */
        val nonAsciiCount: Int,
    ) {
        val hasMarks: Boolean get() = hiddenCount > 0 || nonAsciiCount > 0
        val markCount: Int get() = hiddenCount + nonAsciiCount
    }

    fun render(text: String, nonAscii: Boolean): Marked = render(StyledText(text), nonAscii)

    /**
     * Replaces each marked character of [source] by its label and moves the spans of [source] with the text.
     * A span that covered a replaced character covers its label.
     */
    fun render(source: StyledText, nonAscii: Boolean): Marked {
        val text = source.text
        val out = StringBuilder(text.length + 16)
        // map[i] is the place in `out` of the character i of `text`; map[text.length] is the end.
        val map = IntArray(text.length + 1)
        val added = ArrayList<Span>()
        val marks = LinkedHashMap<Int, Mark>()
        var hidden = 0
        var letters = 0
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            val len = Character.charCount(cp)
            val next = if (i + len < text.length) text.codePointAt(i + len) else -1
            val kind = kindOf(cp, next, nonAscii)
            for (k in 0 until len) map[i + k] = out.length
            if (kind == null) {
                out.appendCodePoint(cp)
            } else {
                val mark = Mark(cp, kind)
                marks.putIfAbsent(cp, mark)
                val start = out.length
                out.append(mark.label)
                added += Span(start, out.length, if (kind.hidden) SpanStyle.MarkHidden else SpanStyle.MarkLetter)
                if (kind.hidden) hidden++ else letters++
            }
            i += len
        }
        map[text.length] = out.length
        val spans = ArrayList<Span>(source.spans.size + added.size)
        for (s in source.spans) spans += Span(map[s.start], map[s.end], s.style)
        spans += added
        return Marked(StyledText(out.toString(), spans), marks.values.toList(), hidden, letters)
    }

    /**
     * The marks of several texts as one report: the command and the field values of its view. The text of the
     * result is empty. The banner above the command names every mark of every part.
     */
    fun combine(parts: List<Marked>): Marked {
        val marks = LinkedHashMap<Int, Mark>()
        for (p in parts) for (m in p.marks) marks.putIfAbsent(m.codePoint, m)
        return Marked(StyledText.EMPTY, marks.values.toList(), parts.sumOf { it.hiddenCount }, parts.sumOf { it.nonAsciiCount })
    }

    // ------------------------------------------------------------ facts about a command

    /** The numbers under the command: statements, lines, bytes and the marks. */
    data class Facts(val statements: Int?, val lines: Int, val bytes: Int, val hiddenCount: Int, val nonAsciiCount: Int) {
        /** The words of the line under the command, one by one. */
        fun words(): List<String> = buildList {
            statements?.let { add(if (it == 1) "1 statement" else "$it statements") }
            add(if (lines == 1) "1 line" else "$lines lines")
            add("$bytes bytes")
            if (hiddenCount > 0) add(if (hiddenCount == 1) "1 hidden" else "$hiddenCount hidden")
            if (nonAsciiCount > 0) add(if (nonAsciiCount == 1) "1 non-ASCII letter" else "$nonAsciiCount non-ASCII letters")
            if (hiddenCount == 0 && nonAsciiCount == 0) add("ASCII only")
        }
    }

    /**
     * The facts of a command. [sqlLike] is true when the language hint is `sql` or missing: only then the
     * number of statements means something. A hint of another language gives no count.
     */
    fun facts(command: String, marked: Marked, sqlLike: Boolean): Facts =
        Facts(
            statements = if (sqlLike) statements(command) else null,
            lines = lines(command),
            bytes = command.toByteArray(Charsets.UTF_8).size,
            hiddenCount = marked.hiddenCount,
            nonAsciiCount = marked.nonAsciiCount,
        )

    fun lines(text: String): Int {
        if (text.isEmpty()) return 0
        val n = text.count { it == '\n' }
        return if (text.endsWith("\n")) n else n + 1
    }

    /**
     * The number of statements: the parts between `;` that hold more than a comment or a space. A `;` inside
     * a quote (`'`, `"`, a backtick) or a comment (`--`, `/* */`, `#` at the start of a line) does not count.
     */
    fun statements(text: String): Int {
        var count = 0
        var has = false
        var i = 0
        val n = text.length
        while (i < n) {
            val c = text[i]
            when {
                c == '\'' || c == '"' || c == '`' -> {
                    has = true
                    i = skipQuoted(text, i, c)
                    continue
                }
                c == '-' && i + 1 < n && text[i + 1] == '-' -> {
                    while (i < n && text[i] != '\n') i++
                    continue
                }
                c == '/' && i + 1 < n && text[i + 1] == '*' -> {
                    val end = text.indexOf("*/", i + 2)
                    i = if (end < 0) n else end + 2
                    continue
                }
                c == '#' && (i == 0 || text[i - 1] == '\n') -> {
                    while (i < n && text[i] != '\n') i++
                    continue
                }
                c == ';' -> {
                    if (has) count++
                    has = false
                }
                !c.isWhitespace() -> has = true
            }
            i++
        }
        if (has) count++
        return count
    }

    /** The index after the quoted part that starts at [from]. A doubled quote is inside the quote. */
    private fun skipQuoted(text: String, from: Int, quote: Char): Int {
        var i = from + 1
        while (i < text.length) {
            if (text[i] == '\\' && quote != '`') {
                i += 2
                continue
            }
            if (text[i] == quote) {
                if (i + 1 < text.length && text[i + 1] == quote) {
                    i += 2
                    continue
                }
                return i + 1
            }
            i++
        }
        return text.length
    }

    // ------------------------------------------------------------ the banner

    /** The text of the banner above the command, or null when the command has no mark. */
    fun banner(marked: Marked): String? {
        if (!marked.hasMarks) return null
        val hidden = marked.marks.filter { it.kind.hidden }.map { it.code }
        val letters = marked.marks.filter { !it.kind.hidden }.map { it.code }
        val count = marked.markCount
        val parts = ArrayList<String>(2)
        if (hidden.isNotEmpty()) parts += hidden.joinToString(", ") + " (hidden)"
        if (letters.isNotEmpty()) parts += letters.joinToString(", ") + " (not ASCII)"
        val head = if (hidden.isNotEmpty()) "Hidden or look-alike characters." else "Characters outside ASCII."
        return "$head ${if (count == 1) "1 mark" else "$count marks"}: ${parts.joinToString(" and ")}. Check each mark."
    }

    /** True when the banner is an alarm: a hidden character is a stronger sign than a non-ASCII letter. */
    fun bannerIsAlarm(marked: Marked): Boolean = marked.hiddenCount > 0

    /** A short line of text with the hidden characters shown as marks (a key name, a service). */
    fun escapeLine(text: String): String = render(text, nonAscii = false).text.text
}
