package app.inbox.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.graphics.Typeface
import android.net.Uri
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextPaint
import android.text.method.LinkMovementMethod
import android.text.style.BackgroundColorSpan
import android.text.style.ClickableSpan
import android.text.style.ForegroundColorSpan
import android.text.style.StrikethroughSpan
import android.text.style.StyleSpan
import android.text.style.TypefaceSpan
import android.view.Gravity
import android.view.View
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.TableLayout
import android.widget.TableRow
import android.widget.TextView
import app.inbox.R
import app.inbox.md.Align
import app.inbox.md.Block
import app.inbox.md.Inline
import app.inbox.md.Markdown

/** Colours used to render Markdown, so the themed sheet and the dark alarm page share the code. */
data class MdPalette(
    val text: Int,
    val strong: Int,
    val muted: Int,
    val codeBg: Int,
    val border: Int,
    val link: Int,
) {
    companion object {
        fun themed(c: Context) = MdPalette(
            c.color(R.color.text), c.color(R.color.text_strong), c.color(R.color.text_muted),
            c.color(R.color.code_bg), c.color(R.color.glass_stroke), c.color(R.color.primary),
        )

        /** The alarm page is always dark. */
        val ALARM = MdPalette(0xD9FFFFFF.toInt(), 0xFFFFFFFF.toInt(), 0x99FFFFFF.toInt(), 0x1FFFFFFF, 0x33FFFFFF, 0xFF7CB6FF.toInt())
    }
}

/** Renders alert details (Markdown) to plain views: no WebView, nothing executed. */
fun Context.markdown(source: String, palette: MdPalette = MdPalette.themed(this), size: Float = SP_M): View {
    val root = lin()
    render(Markdown.parse(source), root, palette, size, first = true)
    return root
}

private fun Context.render(blocks: List<Block>, into: LinearLayout, p: MdPalette, size: Float, first: Boolean) {
    blocks.forEachIndexed { i, b ->
        val top = if (i == 0 && first) 0 else 8
        when (b) {
            is Block.Heading -> into.addView(
                richText(b.inlines, p, if (b.level == 1) scaleStep(size, 1) else size, p.strong, BOLD),
                lp().margins(this, t = if (i == 0) 0 else 12),
            )
            is Block.Paragraph -> into.addView(richText(b.inlines, p, size, p.text, REGULAR), lp().margins(this, t = top))
            is Block.CodeBlock -> into.addView(codeBlock(b, p, size), lp().margins(this, t = top))
            is Block.ListBlock -> into.addView(listBlock(b, p, size), lp().margins(this, t = top))
            is Block.Quote -> {
                val row = lin(vertical = false).apply { gravity = Gravity.NO_GRAVITY }
                row.addView(View(this).apply { setBackgroundColor(p.border) }, lp(dp(3), MATCH))
                val col = lin().apply { setPadding(dp(10), 0, 0, 0) }
                render(b.blocks, col, p.copy(text = p.muted), size, first = true)
                row.addView(col, lp(0, WRAP, 1f))
                into.addView(row, lp().margins(this, t = top))
            }
            Block.Rule -> into.addView(View(this).apply { setBackgroundColor(p.border) }, lp(MATCH, dp(1)).margins(this, t = 12, b = 4))
            is Block.Table -> into.addView(table(b, p, size), lp().margins(this, t = top))
        }
    }
}

private fun Context.richText(inlines: List<Inline>, p: MdPalette, size: Float, color: Int, face: Typeface): TextView {
    val sb = SpannableStringBuilder()
    appendInlines(sb, inlines, p)
    return label(sb, size, color, face).apply {
        setLineSpacing(dpf(3f), 1f)
        setTextIsSelectable(true)
        if (sb.getSpans(0, sb.length, ClickableSpan::class.java).isNotEmpty()) movementMethod = LinkMovementMethod.getInstance()
        setLinkTextColor(p.link)
    }
}

private fun Context.appendInlines(sb: SpannableStringBuilder, inlines: List<Inline>, p: MdPalette) {
    for (i in inlines) {
        val start = sb.length
        when (i) {
            is Inline.Text -> sb.append(i.text)
            Inline.LineBreak -> sb.append('\n')
            is Inline.Code -> {
                sb.append(' ').append(i.text).append(' ')
                sb.setSpan(TypefaceSpan("monospace"), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                sb.setSpan(BackgroundColorSpan(p.codeBg), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                sb.setSpan(ForegroundColorSpan(p.strong), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
            is Inline.Strong -> {
                appendInlines(sb, i.children, p)
                sb.setSpan(StyleSpan(Typeface.BOLD), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
            is Inline.Emphasis -> {
                appendInlines(sb, i.children, p)
                sb.setSpan(StyleSpan(Typeface.ITALIC), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
            is Inline.Strike -> {
                appendInlines(sb, i.children, p)
                sb.setSpan(StrikethroughSpan(), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
            is Inline.Link -> {
                appendInlines(sb, i.children, p)
                val url = i.url
                if (url != null) {
                    sb.setSpan(object : ClickableSpan() {
                        override fun onClick(widget: View) = openUrl(widget.context, url)
                        override fun updateDrawState(ds: TextPaint) {
                            ds.color = p.link
                            ds.isUnderlineText = true
                        }
                    }, start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                }
            }
        }
    }
}

/** Only http(s) links reach here (see [Markdown.safeUrl]); they open in the browser. */
private fun openUrl(context: Context, url: String) {
    try {
        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: ActivityNotFoundException) {
    }
}

private fun Context.codeBlock(b: Block.CodeBlock, p: MdPalette, size: Float): View {
    val box = lin().apply { background = rounded(p.codeBg, dpf(CONTROL_RADIUS), p.border, dp(1)) }
    b.language?.let {
        box.addView(mono(it.uppercase(), SP_XS, p.muted, bold = true).apply {
            letterSpacing = 0.1f
            setPadding(dp(10), dp(7), dp(10), 0)
        })
    }
    val code = mono(b.code, scaleStep(size, -1), p.strong, lines = 0).apply {
        setLineSpacing(dpf(2f), 1f)
        setHorizontallyScrolling(true)
        setTextIsSelectable(true)
        setPadding(dp(10), dp(if (b.language != null) 5 else 9), dp(10), dp(9))
    }
    box.addView(HorizontalScrollView(this).apply {
        isHorizontalScrollBarEnabled = false
        addView(code)
    })
    return box
}

private fun Context.listBlock(b: Block.ListBlock, p: MdPalette, size: Float): View {
    val col = lin()
    b.items.forEachIndexed { k, item ->
        val row = lin(vertical = false).apply { gravity = Gravity.NO_GRAVITY }
        val marker = if (b.ordered) "${b.start + k}." else "•"
        row.addView(mono(marker, size, p.muted).apply { gravity = Gravity.END; setPadding(0, 0, dp(8), 0) },
            lp(dp(if (b.ordered) 26 else 16), WRAP))
        val content = lin()
        render(item, content, p, size, first = true)
        row.addView(content, lp(0, WRAP, 1f))
        col.addView(row, lp().margins(this, t = if (k == 0) 0 else 4))
    }
    return col
}

private fun Context.table(b: Block.Table, p: MdPalette, size: Float): View {
    val t = TableLayout(this).apply {
        background = rounded(0x00000000, dpf(CONTROL_RADIUS), p.border, dp(1))
        setPadding(dp(1), dp(1), dp(1), dp(1))
        isStretchAllColumns = false
    }
    fun cell(inlines: List<Inline>, align: Align, header: Boolean, shaded: Boolean): TextView =
        richText(inlines, p, scaleStep(size, -1), if (header) p.strong else p.text, if (header) BOLD else REGULAR).apply {
            setTextIsSelectable(false)
            gravity = when (align) {
                Align.LEFT -> Gravity.START
                Align.CENTER -> Gravity.CENTER_HORIZONTAL
                Align.RIGHT -> Gravity.END
            }
            setPadding(dp(9), dp(6), dp(9), dp(6))
            background = rounded(if (header || shaded) p.codeBg else 0x00000000, 0f, p.border, 1)
        }
    val head = TableRow(this)
    b.header.forEachIndexed { k, c -> head.addView(cell(c, b.aligns.getOrElse(k) { Align.LEFT }, true, false)) }
    t.addView(head)
    b.rows.forEachIndexed { r, row ->
        val tr = TableRow(this)
        row.forEachIndexed { k, c -> tr.addView(cell(c, b.aligns.getOrElse(k) { Align.LEFT }, false, r % 2 == 1)) }
        t.addView(tr)
    }
    return HorizontalScrollView(this).apply {
        isHorizontalScrollBarEnabled = false
        addView(t)
    }
}
