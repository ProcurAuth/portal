package app.portalinbox.ui

import android.annotation.SuppressLint
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.render.StyledText

/**
 * A scroll box that tells the gate "read to the end" how far the owner scrolled. The box has a largest height. It keeps
 * the touch for itself while it can still scroll in the direction of the finger, so the page behind it does not move.
 */
@SuppressLint("ViewConstructor")
class GateScrollView(context: Context, private val maxHeightPx: Int, private val onMetrics: (scrollY: Int, viewport: Int, content: Int) -> Unit) :
    ScrollView(context) {
    private var downY = 0f

    init {
        isVerticalScrollBarEnabled = true
        isScrollbarFadingEnabled = false
        overScrollMode = OVER_SCROLL_NEVER
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        super.onMeasure(widthMeasureSpec, MeasureSpec.makeMeasureSpec(maxHeightPx, MeasureSpec.AT_MOST))
    }

    override fun onScrollChanged(l: Int, t: Int, oldl: Int, oldt: Int) {
        super.onScrollChanged(l, t, oldl, oldt)
        report()
    }

    override fun onLayout(changed: Boolean, l: Int, t: Int, r: Int, b: Int) {
        super.onLayout(changed, l, t, r, b)
        report()
    }

    private fun report() {
        val child = getChildAt(0) ?: return
        onMetrics(scrollY, height - paddingTop - paddingBottom, child.height)
    }

    override fun onTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downY = e.y
                parent?.requestDisallowInterceptTouchEvent(canScrollVertically(1) || canScrollVertically(-1))
            }
            MotionEvent.ACTION_MOVE -> {
                val dir = if (e.y < downY) 1 else -1
                parent?.requestDisallowInterceptTouchEvent(canScrollVertically(dir))
            }
        }
        return super.onTouchEvent(e)
    }
}

/** A horizontal scroll box for a code block that does not wrap. The page keeps the vertical swipes. */
@SuppressLint("ViewConstructor")
class SideScrollView(context: Context) : HorizontalScrollView(context) {
    private var downX = 0f
    private var downY = 0f

    init {
        isHorizontalScrollBarEnabled = true
        isScrollbarFadingEnabled = false
        overScrollMode = OVER_SCROLL_NEVER
    }

    override fun onTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downX = e.x
                downY = e.y
            }
            MotionEvent.ACTION_MOVE -> {
                val horizontal = kotlin.math.abs(e.x - downX) > kotlin.math.abs(e.y - downY)
                parent?.requestDisallowInterceptTouchEvent(horizontal && (canScrollHorizontally(1) || canScrollHorizontally(-1)))
            }
        }
        return super.onTouchEvent(e)
    }
}

/** Copies a text byte for byte to the clipboard. Gives the text that is on the clipboard now: the caller checks it in tests of the screen. */
fun Context.copyToClipboard(label: String, text: String) {
    val cm = getSystemService(ClipboardManager::class.java)
    cm.setPrimaryClip(ClipData.newPlainText(label, text))
}

/** A small button with an icon and a text, for the Copy of a zone. A 48 dp target. */
fun Context.copyButton(description: String, onClick: () -> Unit): View {
    val row = lin(vertical = false, padH = 10, padV = 0).apply {
        minimumHeight = dp(40)
        background = pressable(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke))
        isClickable = true
        isFocusable = true
        contentDescription = description
        gravity = Gravity.CENTER_VERTICAL
        setOnClickListener { onClick() }
    }
    row.addView(icon(R.drawable.ic_copy, color(R.color.text), 16), lp(dp(16), dp(16)).margins(this, r = 6))
    row.addView(label(getString(R.string.approval_copy), SP_S, color(R.color.text), MEDIUM))
    return row
}

/** The marks of the chips of the head: a small uppercase caption. */
fun Context.caption(value: String, fg: Int = color(R.color.text_strong)): TextView =
    label(value.uppercase(), SP_XS, fg, BOLD).apply { letterSpacing = 0.08f }

/**
 * A code block of an agent: a dashed edge, the language in the head, a Copy button, no line numbers, a muted palette.
 * The code scrolls sideways. The colors never replace the marks of hidden characters.
 */
fun Context.agentCodeBlock(language: String, code: StyledText, onCopy: (String) -> Unit, original: String): View {
    val box = lin().apply {
        background = dashedBox(color(R.color.code_bg), color(R.color.card_stroke), dpf(CARD_RADIUS))
        clipToOutline = true
    }
    val head = lin(vertical = false, padH = 8, padV = 2).apply { gravity = Gravity.CENTER_VERTICAL }
    head.addView(caption(if (language == "plain") "code" else language, color(R.color.text_muted)), lp(0, WRAP, 1f))
    head.addView(copyButton(getString(R.string.approval_copy_description)) { onCopy(original) })
    box.addView(head)
    box.addView(divider())
    val text = mono(SpanApplier(this, CodePalette.AGENT).apply(code), SP_S, color(R.color.text_muted), lines = Int.MAX_VALUE).apply {
        setHorizontallyScrolling(true)
        setTextIsSelectable(false)
        setPadding(dp(10), dp(8), dp(10), dp(10))
        textDirection = View.TEXT_DIRECTION_LTR
        maxLines = Int.MAX_VALUE
        ellipsize = null
        movementMethod = null
    }
    box.addView(SideScrollView(this).apply { addView(text, ViewGroup.LayoutParams(WRAP, WRAP)) }, lp())
    return box
}

/** A box with a dashed edge: the look of the text of a third party. */
fun Context.dashedBox(fill: Int, stroke: Int, radius: Float): GradientDrawable =
    GradientDrawable().apply {
        setColor(fill)
        cornerRadius = radius
        setStroke(dp(1), stroke, dpf(4f), dpf(3f))
    }


/** A strip with a colored edge and a text that the screen changes: an invalid request, a closed item, a result. */
class NoticeBanner(context: Context) : LinearLayout(context) {
    private val bar = View(context)
    private val iconView = context.icon(R.drawable.ic_warning, context.color(R.color.warning), 16)
    private val text = context.label("", SP_S, context.color(R.color.text))

    init {
        orientation = HORIZONTAL
        addView(bar, lp(dp(3), MATCH))
        addView(iconView, lp(dp(16), dp(16)).margins(context, l = 10, r = 8).apply { gravity = Gravity.CENTER_VERTICAL })
        addView(text.apply { setPadding(0, dp(9), dp(10), dp(9)) }, lp(0, WRAP, 1f))
        clipToOutline = true
        outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
        visibility = GONE
    }

    private fun dp(v: Int) = context.dp(v)

    /** Shows [message] with the colors [fg] and [bg], or hides the strip when [message] is null. */
    fun show(message: String?, fg: Int, bg: Int, iconRes: Int = R.drawable.ic_warning) {
        if (message == null) {
            visibility = GONE
            return
        }
        background = rounded(bg, context.dpf(CARD_RADIUS), fg and 0x4DFFFFFF, context.dp(1))
        bar.setBackgroundColor(fg)
        iconView.setImageResource(iconRes)
        iconView.imageTintList = android.content.res.ColorStateList.valueOf(fg)
        text.text = message
        visibility = VISIBLE
    }

}
