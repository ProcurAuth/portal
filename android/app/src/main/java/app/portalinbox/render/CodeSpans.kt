package app.portalinbox.render

import org.json.JSONArray
import org.json.JSONException

/**
 * Colors a code text. The tokens come from the Rust code (`Native.highlightTokens`): a JSON array
 * `[[start, end, "class"], ...]` with offsets in UTF-16 units. This class checks the answer and turns
 * it into spans. The color never changes the text: the text of the result is the text that came in.
 *
 * A language that is null or not in the closed list gives no color. A bad answer (an offset out of
 * range, an overlap, a cut inside a surrogate pair, an unknown class) also gives no color: the text
 * is shown plain, never half colored.
 */
object CodeSpans {
    /** The tokens of a language and a text, as JSON text. The real one is `Native.highlightTokens`. */
    fun interface Tokenizer {
        fun tokens(language: String, text: String): String
    }

    /** The closed list of the languages that have color. */
    val LANGUAGES = listOf("sql", "json", "yaml", "toml", "bash", "python", "rust", "javascript", "typescript", "diff")

    fun build(language: String?, text: String, tokenizer: Tokenizer): StyledText {
        if (language == null || language !in LANGUAGES || text.isEmpty()) return StyledText(text)
        val json = try {
            tokenizer.tokens(language, text)
        } catch (e: RuntimeException) {
            return StyledText(text)
        }
        return StyledText(text, spans(text, json) ?: emptyList())
    }

    /** The spans of an answer, or null when the answer breaks a rule. Plain tokens make no span. */
    internal fun spans(text: String, json: String): List<Span>? {
        val array = try {
            JSONArray(json)
        } catch (e: JSONException) {
            return null
        }
        if (array.length() > text.length) return null
        val out = ArrayList<Span>(array.length())
        var at = 0
        for (n in 0 until array.length()) {
            val entry = array.optJSONArray(n) ?: return null
            if (entry.length() != 3) return null
            val start = (entry.opt(0) as? Int) ?: return null
            val end = (entry.opt(1) as? Int) ?: return null
            val cls = TokenClass.of(entry.opt(2) as? String ?: return null) ?: return null
            if (start < at || end <= start || end > text.length) return null
            if (cutsPair(text, start) || cutsPair(text, end)) return null
            at = end
            if (cls == TokenClass.PLAIN) continue
            val last = out.lastOrNull()
            if (last != null && last.end == start && last.style == SpanStyle.Token(cls)) {
                out[out.size - 1] = last.copy(end = end)
            } else {
                out += Span(start, end, SpanStyle.Token(cls))
            }
        }
        return out
    }

    private fun cutsPair(text: String, at: Int): Boolean =
        at in 1 until text.length && Character.isHighSurrogate(text[at - 1]) && Character.isLowSurrogate(text[at])
}
