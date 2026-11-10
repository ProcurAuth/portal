package app.portalinbox.ui

import android.content.Context
import android.graphics.Typeface
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.core.NativeRender
import app.portalinbox.proto.ExplanationFormat
import app.portalinbox.render.MarkdownSpans
import app.portalinbox.render.MdBlock

/**
 * The zone "Agent explanation" of the approval screen and of the question screen: the text of a third party. A dashed edge, the
 * warning icon, the label "Not checked", the marks for hidden characters, no link, no select, a fold after 400 characters,
 * and the fixed sentence. A Markdown text shows with the closed subset of the parser that the server uses too. All the
 * decisions are in [ExplanationText]; this class only draws.
 */
class ExplanationZone(
    private val context: Context,
    /** Called with the words "Copied" after the Copy button of a block of code. The screen shows a snackbar. */
    private val onCodeCopied: (String) -> Unit,
    private val fixedSentence: String = ExplanationText.FIXED_SENTENCE,
) {
    val view: LinearLayout = context.lin()
    var expanded = false
    var rawMarkdown = false

    private var explanation: String? = null
    private var format: ExplanationFormat? = null

    /** Draws the zone for [text]. A new text keeps the choices of the owner (fold, plain view) until the screen is built again. */
    fun show(text: String?, textFormat: ExplanationFormat?) {
        explanation = text
        format = textFormat
        redraw()
    }

    private fun redraw() = with(context) {
        view.removeAllViews()
        val shownView = ExplanationText.view(explanation, format, expanded, rawMarkdown)
        val zone = lin().apply {
            background = dashedBox(color(R.color.card), color(R.color.card_stroke), dpf(CARD_RADIUS))
            clipToOutline = true
            outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
        }
        val head = lin(vertical = false, padH = 12, padV = 8).apply { gravity = Gravity.CENTER_VERTICAL }
        head.addView(icon(R.drawable.ic_warning, color(R.color.warning), 16), lp(dp(16), dp(16)).margins(this, r = 8))
        head.addView(caption(getString(R.string.approval_explanation)))
        if (shownView is ExplanationText.View.Markdown) {
            head.addView(caption(getString(R.string.approval_markdown), color(R.color.text_muted)), lp().margins(this, l = 8).apply { width = WRAP })
        }
        head.addView(View(this), lp(0, 1, 1f))
        head.addView(caption(getString(R.string.approval_not_checked), color(R.color.warning)))
        zone.addView(head)
        zone.addView(dashedDivider())

        when (shownView) {
            ExplanationText.View.Absent -> zone.addView(
                label(ExplanationText.ABSENT, SP_M, color(R.color.text_muted)).apply { setPadding(dp(12), dp(12), dp(12), dp(12)) },
            )
            is ExplanationText.View.Plain -> {
                val shown = ExplanationText.quoted(shownView.shown + if (shownView.folded) "…" else "")
                val marked = VisibleText.render(shown, nonAscii = false)
                zone.addView(
                    label(SpanApplier(this).apply(marked.text), SP_L, color(R.color.text)).apply {
                        setTypeface(REGULAR, Typeface.ITALIC)
                        setTextIsSelectable(false)
                        isLongClickable = false
                        setPadding(dp(12), dp(12), dp(12), dp(8))
                        setLineSpacing(dpf(2f), 1f)
                    },
                )
                if (shownView.folded) {
                    zone.addView(textLink(getString(R.string.approval_show_all)) {
                        expanded = true
                        redraw()
                    }, lp(WRAP, WRAP).margins(this, l = 4, b = 4))
                }
                if (format == ExplanationFormat.MARKDOWN && rawMarkdown) {
                    zone.addView(textLink(getString(R.string.approval_show_markdown)) {
                        rawMarkdown = false
                        redraw()
                    }, lp(WRAP, WRAP).margins(this, l = 4, b = 4))
                }
            }
            is ExplanationText.View.Markdown -> zone.addView(markdownBox(shownView.source))
        }
        if (shownView != ExplanationText.View.Absent) {
            zone.addView(dashedDivider())
            zone.addView(label(fixedSentence, SP_S, color(R.color.text_muted)).apply { setPadding(dp(12), dp(8), dp(12), dp(8)) })
        }
        view.addView(zone)
    }

    private fun markdownBox(source: String): View = with(context) {
        val doc = MarkdownSpans.build(source, NativeRender, NativeRender)
        val inner = lin(padH = 12, padV = 8)
        val applier = SpanApplier(this, CodePalette.AGENT)
        for (block in doc.blocks) {
            when (block) {
                is MdBlock.Flow -> inner.addView(
                    label(applier.apply(VisibleText.render(block.text, nonAscii = false).text), SP_M, color(R.color.text)).apply {
                        setTextIsSelectable(false)
                        isLongClickable = false
                        movementMethod = null
                        setLineSpacing(dpf(2f), 1f)
                    },
                    lp().margins(this, b = 4),
                )
                is MdBlock.Code -> {
                    val shown = VisibleText.render(block.text, nonAscii = false).text
                    inner.addView(
                        agentCodeBlock(block.lang, shown, { text ->
                            copyToClipboard("code", text)
                            onCodeCopied(getString(R.string.approval_code_copied))
                        }, block.text.text),
                        lp().margins(this, t = 4, b = 8),
                    )
                }
            }
        }
        val box = GateScrollView(this, dp(280)) { _, _, _ -> }
        box.addView(inner, android.view.ViewGroup.LayoutParams(MATCH, WRAP))
        val wrap = lin()
        wrap.addView(box, lp())
        if (doc.fallback) wrap.addView(label("Shown as plain text: it is too long or too deep.", SP_S, color(R.color.text_muted)).apply { setPadding(dp(12), 0, dp(12), dp(6)) })
        wrap.addView(dashedDivider())
        val foot = lin(vertical = false, padH = 12, padV = 4).apply { gravity = Gravity.CENTER_VERTICAL }
        foot.addView(label(getString(R.string.approval_markdown_foot), SP_S, color(R.color.text_muted)), lp(0, WRAP, 1f))
        foot.addView(textLink(getString(R.string.approval_show_plain)) {
            rawMarkdown = true
            redraw()
        })
        wrap.addView(foot)
        wrap
    }

    private fun Context.dashedDivider(): View = View(this).apply {
        background = android.graphics.drawable.GradientDrawable().apply {
            setStroke(dp(1), color(R.color.card_stroke), dpf(4f), dpf(3f))
        }
        setLayerType(View.LAYER_TYPE_SOFTWARE, null)
        layoutParams = LinearLayout.LayoutParams(MATCH, dp(1))
    }

    private fun Context.textLink(value: String, onClick: () -> Unit): TextView =
        label(value, SP_M, color(R.color.primary), MEDIUM).apply {
            minHeight = dp(48)
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(12), 0, dp(12), 0)
            isClickable = true
            isFocusable = true
            background = rowRipple()
            setOnClickListener { onClick() }
        }
}
