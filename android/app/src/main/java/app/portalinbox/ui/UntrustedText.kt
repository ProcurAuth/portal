package app.portalinbox.ui

import app.portalinbox.render.StyledText

/**
 * The path for each text that a client or an agent sent: what it says, its client, its address range. Pure Kotlin.
 *
 * Nothing is removed and nothing is refused on the phone. A hidden character (control, bidi, zero width, space) shows as a mark
 * like `<U+202E>`. A letter outside ASCII also shows with its code, because it can look like an ASCII letter (a Cyrillic "a" in
 * "billing-bot"). It is the marking path of the command ([VisibleText]), with the look-alike marks on. The screen turns the
 * [StyledText] into a `Spannable` with `SpanApplier`.
 */
object UntrustedText {
    /** The text with the marks in place. */
    fun mark(text: String): StyledText = VisibleText.render(text, nonAscii = true).text

    /** The marked text as plain characters, for a screen reader and for a place that takes no spans. */
    fun plain(text: String): String = mark(text).text

    /** True when the text has a hidden or look-alike character: the screen then says so next to the box. */
    fun hasMarks(text: String): Boolean = VisibleText.render(text, nonAscii = true).hasMarks

    /** The text between plain quotation marks, marked. The quotes are ASCII: they are not a part of what the client said. */
    fun quoted(text: String): StyledText {
        val m = mark(text)
        val prefix = "\""
        val shifted = m.spans.map { it.copy(start = it.start + prefix.length, end = it.end + prefix.length) }
        return StyledText(prefix + m.text + "\"", shifted)
    }
}
