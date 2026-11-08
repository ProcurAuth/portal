package app.portalinbox.core

import app.portalinbox.render.CodeSpans
import app.portalinbox.render.MarkdownSpans

/** The Markdown tree and the code tokens of `inbox-core`, through JNI. The JVM tests use their own functions. */
object NativeRender : MarkdownSpans.Parser, CodeSpans.Tokenizer {
    override fun parse(text: String): String = Native.markdownParse(text)

    override fun tokens(language: String, text: String): String = Native.highlightTokens(language, text)
}
