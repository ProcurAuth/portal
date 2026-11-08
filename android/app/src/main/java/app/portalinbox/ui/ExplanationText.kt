package app.portalinbox.ui

import app.portalinbox.proto.ExplanationFormat

/**
 * What the zone "Agent explanation" shows (CONF 4.9). Pure Kotlin.
 *
 * The explanation is the text of a third party. It never changes what will run, and the screen says so with
 * a fixed sentence. A missing or empty explanation shows "No explanation given".
 */
object ExplanationText {
    const val FIXED_SENTENCE = "This explanation does not change what will run."
    const val ABSENT = "No explanation given"
    const val NOT_CHECKED = "Not checked"

    /** A plain explanation longer than this folds. */
    const val FOLD_CHARS = 400

    /** The 8000 characters of the protocol never fold in a scroll box: only a plain text folds. */
    sealed interface View {
        data object Absent : View

        /** A plain text. [shown] is the part that shows, [folded] is true when more text waits behind "Show all". */
        data class Plain(val shown: String, val folded: Boolean) : View

        /** A Markdown text. The screen builds it with `MarkdownSpans` and shows it in a scroll box. */
        data class Markdown(val source: String) : View
    }

    /**
     * The view of an explanation. [expanded] is true after "Show all". [rawMarkdown] is true after "Show plain text":
     * the source of the Markdown shows as plain text.
     */
    fun view(explanation: String?, format: ExplanationFormat?, expanded: Boolean, rawMarkdown: Boolean = false): View {
        if (explanation == null || explanation.isBlank()) return View.Absent
        if (format == ExplanationFormat.MARKDOWN && !rawMarkdown) return View.Markdown(explanation)
        return fold(explanation, expanded)
    }

    /** The first [FOLD_CHARS] characters of a plain text, cut on a code point (never inside a pair). */
    fun fold(text: String, expanded: Boolean): View.Plain {
        if (expanded || text.codePointCount(0, text.length) <= FOLD_CHARS) return View.Plain(text, false)
        val end = text.offsetByCodePoints(0, FOLD_CHARS)
        return View.Plain(text.substring(0, end), true)
    }

    /** The plain explanation between quotation marks, as the zone shows it. */
    fun quoted(text: String): String = "“$text”"
}
