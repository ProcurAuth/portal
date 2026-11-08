package app.portalinbox.render

/** The class of a token of code, as `inbox_core::render::highlight::Class` names it. */
enum class TokenClass(val wire: String) {
    KEYWORD("keyword"), STRING("string"), NUMBER("number"), COMMENT("comment"), FUNCTION("function"),
    TYPE("type"), OPERATOR("operator"), PUNCTUATION("punctuation"), PLAIN("plain"),
    ADD("add"), DEL("del"), HUNK("hunk"), META("meta");

    companion object {
        fun of(wire: String): TokenClass? = entries.firstOrNull { it.wire == wire }
    }
}

/**
 * What a range of a [StyledText] looks like. The names say the role, never a color: the screen maps
 * a role to a color of the theme, so the pure classes and their tests know nothing of Android.
 */
sealed interface SpanStyle {
    data class Token(val cls: TokenClass) : SpanStyle

    /** A heading of level 1 to 3: a bold line. */
    data class Heading(val level: Int) : SpanStyle

    data object Strong : SpanStyle
    data object Emphasis : SpanStyle

    /** Inline code: a fixed font. */
    data object InlineCode : SpanStyle

    /** The part of a link that the phone adds: ` (host)`. A link is never active. */
    data object LinkHost : SpanStyle

    /** The box of a task item. Read only. */
    data object TaskBox : SpanStyle

    /** A block quote: a bar on the left. */
    data object Quote : SpanStyle

    /** A small gap between two blocks: the character of the range is a line break. */
    data object Gap : SpanStyle

    /** Indent of the lines of a paragraph. With [hanging], the lines after the first sit one step deeper. */
    data class Indent(val level: Int, val hanging: Boolean) : SpanStyle

    /** A hidden character made visible: `<U+202E>`. */
    data object MarkHidden : SpanStyle

    /** A non-ASCII letter made visible with its code. */
    data object MarkLetter : SpanStyle
}

/** A range of the text, in UTF-16 units, as Kotlin indexes a string. [end] is not in the range. */
data class Span(val start: Int, val end: Int, val style: SpanStyle)

/** A text and the styled ranges over it. Pure data: the screen turns it into a `Spannable`. */
data class StyledText(val text: String, val spans: List<Span> = emptyList()) {
    val length: Int get() = text.length

    init {
        for (s in spans) require(s.start in 0..s.end && s.end <= text.length) { "span out of range: $s for ${text.length}" }
    }

    companion object {
        val EMPTY = StyledText("")
    }
}
