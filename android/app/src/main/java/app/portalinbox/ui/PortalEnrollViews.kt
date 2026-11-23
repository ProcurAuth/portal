package app.portalinbox.ui

import android.content.Context
import android.content.res.Configuration
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.text.SpannableString
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.StyleSpan
import android.view.Gravity
import android.view.View
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.portal.EnrollPlan
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PortalPalette

/*
 * Views of the identity of a portal at enrollment: the monogram square, the color swatch and the picker, and the address text
 * with the changed letters marked. The other screens of the multi-portal lots (home list, approval, alarm) use the same views, so the
 * portal looks the same everywhere. A screen never draws a portal from `Server.reportedName`: it takes a [PortalIdentity].
 */

/** True when the context shows the dark theme. The themed context of a screen already holds the theme that the user chose. */
fun Context.isNight(): Boolean =
    (resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES

/** The color of a portal in the theme of this context. */
fun Context.portalColor(identity: PortalIdentity): Int = identity.color(isNight())

/** The color of an index of the palette in the theme of this context. */
fun Context.portalColor(colorIndex: Int): Int = PortalPalette.color(colorIndex, isNight())

/**
 * The monogram: the first two letters of the nickname on a square of the portal color. It is the second cue for a person who does not
 * see color. [sizeDp] is the side of the square.
 */
fun Context.monogramSquare(identity: PortalIdentity, sizeDp: Int = 40): FrameLayout = FrameLayout(this).apply {
    background = rounded(portalColor(identity), dpf(sizeDp * 0.22f))
    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
    val text = label(identity.monogram, if (sizeDp >= 36) SP_L else SP_XS, identity.onColor(isNight()), BOLD, lines = 1)
    addView(text, FrameLayout.LayoutParams(WRAP, WRAP, Gravity.CENTER))
    layoutParams = LinearLayout.LayoutParams(dp(sizeDp), dp(sizeDp))
}

/** The "New" mark of a portal that was added in the last 24 hours. */
fun Context.newMark(): TextView = chip(getString(R.string.portal_new), color(R.color.info), color(R.color.info_soft))

/** The address with the changed letters in bold danger color. [ranges] are indices of [text]. */
fun Context.markedAddress(text: String, ranges: List<IntRange>): CharSequence {
    if (ranges.isEmpty()) return text
    val span = SpannableString(text)
    val danger = color(R.color.danger)
    for (r in ranges) {
        if (r.first < 0 || r.last >= text.length) continue
        span.setSpan(ForegroundColorSpan(danger), r.first, r.last + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        span.setSpan(StyleSpan(android.graphics.Typeface.BOLD), r.first, r.last + 1, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
    }
    return span
}

/** One swatch of the picker: the color, crossed out when another portal uses it, a ring and a check when chosen. */
class ColorSwatch(context: Context, private val colorIndex: Int, private val usedBy: String?, private var chosen: Boolean) : View(context) {
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG)
    private val line = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; strokeCap = Paint.Cap.ROUND }
    private val rect = RectF()
    private val check = Path()

    init {
        isFocusable = true
        isClickable = usedBy == null
        val name = PortalPalette.name(colorIndex).replaceFirstChar { it.uppercase() }
        contentDescription = if (usedBy != null) context.getString(R.string.enroll_color_used, name, usedBy) else name
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            stateDescription = context.getString(if (chosen) R.string.a11y_selected else R.string.a11y_not_selected)
        }
        isSelected = chosen
    }

    override fun onDraw(canvas: Canvas) {
        val c = context
        val night = c.isNight()
        val base = PortalPalette.color(colorIndex, night)
        val w = width.toFloat()
        val h = height.toFloat()
        val r = c.dpf(10f)
        val inset = c.dpf(2f)
        rect.set(inset, inset, w - inset, h - inset)
        // A used color is dim: the portal that has it shows the color, this swatch only tells it is taken.
        fill.color = if (usedBy != null) (base and 0x00FFFFFF) or 0x4D000000 else base
        canvas.drawRoundRect(rect, r, r, fill)
        if (usedBy != null) {
            line.color = c.color(R.color.text)
            line.strokeWidth = c.dpf(1.6f)
            canvas.drawLine(w * 0.28f, h * 0.72f, w * 0.72f, h * 0.28f, line)
        }
        if (chosen) {
            line.color = c.color(R.color.text_strong)
            line.strokeWidth = c.dpf(2f)
            rect.set(c.dpf(0.5f), c.dpf(0.5f), w - c.dpf(0.5f), h - c.dpf(0.5f))
            canvas.drawRoundRect(rect, r + c.dpf(2f), r + c.dpf(2f), line)
            line.color = PortalPalette.onColor(night)
            line.strokeWidth = c.dpf(2.4f)
            check.reset()
            check.moveTo(w * 0.30f, h * 0.52f)
            check.lineTo(w * 0.45f, h * 0.67f)
            check.lineTo(w * 0.72f, h * 0.36f)
            canvas.drawPath(check, line)
        }
    }
}

/**
 * The color picker: the 10 swatches in rows. A color that another portal uses is crossed out and cannot be chosen. [chosen] is the
 * index that applies now (or null). [onPick] gets the index of a free swatch.
 */
fun Context.colorPicker(choices: List<EnrollPlan.ColorChoice>, chosen: Int?, onPick: (Int) -> Unit): LinearLayout {
    val box = lin()
    val size = 44
    val gap = 8
    val perRow = ((resources.configuration.screenWidthDp - 48) / (size + gap)).coerceIn(4, 10)
    var rowView: LinearLayout? = null
    choices.forEachIndexed { i, choice ->
        if (i % perRow == 0) {
            rowView = lin(vertical = false)
            box.addView(rowView, lp().margins(this, t = if (i == 0) 0 else gap))
        }
        val swatch = ColorSwatch(this, choice.index, choice.usedBy?.nickname, choice.index == chosen)
        if (choice.free) swatch.setOnClickListener { onPick(choice.index) }
        rowView!!.addView(swatch, lp(dp(size), dp(size)).margins(this, r = gap))
    }
    return box
}
