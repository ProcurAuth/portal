package app.portalinbox.ui

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import android.text.Layout
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.BackgroundColorSpan
import android.text.style.ForegroundColorSpan
import android.text.style.LeadingMarginSpan
import android.text.style.QuoteSpan
import android.text.style.RelativeSizeSpan
import android.text.style.StyleSpan
import android.text.style.TypefaceSpan
import app.portalinbox.R
import app.portalinbox.render.SpanStyle
import app.portalinbox.render.StyledText
import app.portalinbox.render.TokenClass

/** Which palette colors code: the command (the colors of the review) or the text of an agent (a muted set). */
enum class CodePalette { COMMAND, AGENT }

/** Turns a [StyledText] into a `Spannable`. The pure builders decide the roles. This class only picks the colors and the spans. */
class SpanApplier(private val context: Context, private val palette: CodePalette = CodePalette.AGENT) {
    private fun c(id: Int) = context.getColor(id)

    private fun tokenColor(cls: TokenClass): Int = when (palette) {
        CodePalette.COMMAND -> when (cls) {
            TokenClass.KEYWORD -> c(R.color.code_keyword)
            TokenClass.STRING -> c(R.color.code_string)
            TokenClass.NUMBER -> c(R.color.code_number)
            TokenClass.COMMENT -> c(R.color.code_comment)
            TokenClass.FUNCTION -> c(R.color.code_function)
            TokenClass.TYPE -> c(R.color.code_type)
            TokenClass.OPERATOR -> c(R.color.code_operator)
            TokenClass.PUNCTUATION -> c(R.color.code_punct)
            TokenClass.ADD -> c(R.color.code_add)
            TokenClass.DEL -> c(R.color.code_del)
            TokenClass.HUNK -> c(R.color.code_hunk)
            TokenClass.META -> c(R.color.code_meta)
            TokenClass.PLAIN -> c(R.color.text)
        }
        CodePalette.AGENT -> when (cls) {
            TokenClass.KEYWORD -> c(R.color.agent_keyword)
            TokenClass.STRING -> c(R.color.agent_string)
            TokenClass.NUMBER -> c(R.color.agent_number)
            TokenClass.COMMENT -> c(R.color.agent_comment)
            TokenClass.FUNCTION -> c(R.color.agent_function)
            TokenClass.TYPE -> c(R.color.agent_type)
            TokenClass.OPERATOR -> c(R.color.agent_operator)
            TokenClass.PUNCTUATION -> c(R.color.agent_punct)
            TokenClass.ADD -> c(R.color.agent_add)
            TokenClass.DEL -> c(R.color.agent_del)
            TokenClass.HUNK -> c(R.color.agent_hunk)
            TokenClass.META -> c(R.color.agent_meta)
            TokenClass.PLAIN -> c(R.color.text_muted)
        }
    }

    private fun tokenBackground(cls: TokenClass): Int? = when (palette) {
        CodePalette.COMMAND -> when (cls) {
            TokenClass.ADD -> c(R.color.code_add_bg)
            TokenClass.DEL -> c(R.color.code_del_bg)
            TokenClass.HUNK -> c(R.color.code_hunk_bg)
            else -> null
        }
        CodePalette.AGENT -> when (cls) {
            TokenClass.ADD -> c(R.color.agent_add_bg)
            TokenClass.DEL -> c(R.color.agent_del_bg)
            TokenClass.HUNK -> c(R.color.agent_hunk_bg)
            else -> null
        }
    }

    private fun dp(v: Int) = (v * context.resources.displayMetrics.density + 0.5f).toInt()

    fun apply(styled: StyledText): SpannableStringBuilder {
        val sb = SpannableStringBuilder(styled.text)
        for (s in styled.spans) {
            if (s.start >= s.end) continue
            val exclusive = Spanned.SPAN_EXCLUSIVE_EXCLUSIVE
            when (val st = s.style) {
                is SpanStyle.Token -> {
                    sb.setSpan(ForegroundColorSpan(tokenColor(st.cls)), s.start, s.end, exclusive)
                    tokenBackground(st.cls)?.let { sb.setSpan(BackgroundColorSpan(it), s.start, s.end, exclusive) }
                    if (st.cls == TokenClass.COMMENT) sb.setSpan(StyleSpan(Typeface.ITALIC), s.start, s.end, exclusive)
                    if (st.cls == TokenClass.META) sb.setSpan(StyleSpan(Typeface.BOLD), s.start, s.end, exclusive)
                }
                is SpanStyle.Heading -> {
                    sb.setSpan(StyleSpan(Typeface.BOLD), s.start, s.end, exclusive)
                    val size = when (st.level) { 1 -> 1.18f; 2 -> 1.08f; else -> 1.0f }
                    if (size != 1.0f) sb.setSpan(RelativeSizeSpan(size), s.start, s.end, exclusive)
                    if (st.level == 3) sb.setSpan(ForegroundColorSpan(c(R.color.text_muted)), s.start, s.end, exclusive)
                }
                SpanStyle.Strong -> sb.setSpan(StyleSpan(Typeface.BOLD), s.start, s.end, exclusive)
                SpanStyle.Emphasis -> sb.setSpan(StyleSpan(Typeface.ITALIC), s.start, s.end, exclusive)
                SpanStyle.InlineCode -> {
                    sb.setSpan(TypefaceSpan("monospace"), s.start, s.end, exclusive)
                    sb.setSpan(RelativeSizeSpan(0.9f), s.start, s.end, exclusive)
                    sb.setSpan(BackgroundColorSpan(c(R.color.bg_hover)), s.start, s.end, exclusive)
                }
                SpanStyle.LinkHost -> sb.setSpan(ForegroundColorSpan(c(R.color.text_muted)), s.start, s.end, exclusive)
                SpanStyle.TaskBox -> {
                    sb.setSpan(ForegroundColorSpan(c(R.color.text_muted)), s.start, s.end, exclusive)
                    sb.setSpan(RelativeSizeSpan(1.15f), s.start, s.end, exclusive)
                }
                SpanStyle.Quote -> {
                    sb.setSpan(ForegroundColorSpan(c(R.color.text_muted)), s.start, s.end, exclusive)
                    sb.setSpan(QuoteSpan(c(R.color.text_subtle)), s.start, paragraphEnd(sb, s.end), exclusive)
                }
                SpanStyle.Gap -> sb.setSpan(RelativeSizeSpan(0.45f), s.start, s.end, exclusive)
                is SpanStyle.Indent -> {
                    val first = dp(14 * (st.level - 1))
                    val rest = if (st.hanging) first + dp(18) else first
                    sb.setSpan(LeadingMarginSpan.Standard(first, rest), s.start, paragraphEnd(sb, s.end), exclusive)
                }
                SpanStyle.MarkHidden -> mark(sb, s.start, s.end, c(R.color.danger), c(R.color.danger_soft))
                SpanStyle.MarkLetter -> mark(sb, s.start, s.end, c(R.color.warning), c(R.color.warning_soft))
            }
        }
        return sb
    }

    /** A chip for a mark: bold fixed font on a tinted background, so a hidden character cannot pass for text. */
    private fun mark(sb: SpannableStringBuilder, start: Int, end: Int, fg: Int, bg: Int) {
        val f = Spanned.SPAN_EXCLUSIVE_EXCLUSIVE
        sb.setSpan(ForegroundColorSpan(fg), start, end, f)
        sb.setSpan(BackgroundColorSpan(bg), start, end, f)
        sb.setSpan(TypefaceSpan("monospace"), start, end, f)
        sb.setSpan(StyleSpan(Typeface.BOLD), start, end, f)
        sb.setSpan(RelativeSizeSpan(0.85f), start, end, f)
    }

    /** A paragraph span ends after the line break of its last line. */
    private fun paragraphEnd(sb: CharSequence, end: Int): Int {
        if (end >= sb.length) return sb.length
        if (end > 0 && sb[end - 1] == '\n') return end
        val nl = sb.indexOf('\n', end)
        return if (nl < 0) sb.length else nl + 1
    }

    /**
     * Puts the number of each line in the left margin. A line that wraps keeps its number on the first row only,
     * so the numbers stay aligned with the text.
     */
    fun numberLines(sb: SpannableStringBuilder, textSizePx: Float) {
        val lines = sb.count { it == '\n' } + if (sb.isNotEmpty() && sb.last() != '\n') 1 else 0
        if (lines == 0) return
        val digits = lines.toString().length
        val gutter = (digits * textSizePx * 0.62f).toInt() + dp(10)
        var start = 0
        var n = 1
        while (start < sb.length) {
            val nl = sb.indexOf('\n', start)
            val end = if (nl < 0) sb.length else nl + 1
            sb.setSpan(LineNumberSpan(n, gutter, c(R.color.text_subtle), dp(6)), start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            start = end
            n++
        }
    }
}

/** The number of a line, drawn in the left margin of its first row. */
class LineNumberSpan(private val number: Int, private val width: Int, private val color: Int, private val gap: Int) : LeadingMarginSpan {
    override fun getLeadingMargin(first: Boolean): Int = width

    override fun drawLeadingMargin(
        c: Canvas, p: Paint, x: Int, dir: Int, top: Int, baseline: Int, bottom: Int,
        text: CharSequence, start: Int, end: Int, first: Boolean, layout: Layout?,
    ) {
        if (!first) return
        val oldColor = p.color
        val oldAlign = p.textAlign
        p.color = color
        p.textAlign = Paint.Align.RIGHT
        c.drawText(number.toString(), (x + dir * (width - gap)).toFloat(), baseline.toFloat(), p)
        p.color = oldColor
        p.textAlign = oldAlign
    }
}
