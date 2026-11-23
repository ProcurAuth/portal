package app.portalinbox.ui

import android.content.Context
import android.content.res.Configuration
import android.graphics.drawable.GradientDrawable
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.StyleSpan
import android.view.Gravity
import android.view.View
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.portal.FilterStrip
import app.portalinbox.portal.HomePortal
import app.portalinbox.portal.OfflineSummary
import app.portalinbox.portal.PortalHealth
import app.portalinbox.portal.PortalIdentity

/*
 * The views of the multi-portal home list. They draw a portal only from [PortalIdentity]:
 * the nickname, the color and the monogram are local, never the name that the portal sends. Colors of the portal come from
 * `PortalPalette` through the identity, not from the resources, so the dark and the light value are the ones that were checked
 * for contrast.
 */

/** True when this context shows the dark theme (the theme of the user, see `themedContext`). */
fun Context.darkTheme(): Boolean =
    (resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES

private fun withAlpha(argb: Int, alpha: Int): Int = (alpha shl 24) or (argb and 0x00FFFFFF)

/** The square with the monogram: the second cue for a person who does not see color. */
fun Context.monogram(p: PortalIdentity, night: Boolean = darkTheme(), sizeDp: Int = 20): TextView =
    label(p.monogram, SP_XS, p.onColor(night), BOLD, lines = 1).apply {
        gravity = Gravity.CENTER
        background = rounded(p.color(night), dpf(CHIP_RADIUS))
        layoutParams = LinearLayout.LayoutParams(dp(sizeDp), dp(sizeDp))
        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
    }

/** A shape with the portal color: [dashed] marks a portal that is down. */
private fun Context.tint(fill: Int, stroke: Int, radiusDp: Float, dashed: Boolean): GradientDrawable = GradientDrawable().apply {
    setColor(fill)
    cornerRadius = dpf(radiusDp)
    if (dashed) setStroke(dp(1), stroke, dpf(3f), dpf(2.5f)) else setStroke(dp(1), stroke)
}

/** The small "New" mark of a portal added in the last 24 hours. */
private fun Context.newMark(p: PortalIdentity, night: Boolean): TextView =
    label(getString(R.string.portal_new), SP_XS, p.onColor(night), BOLD, lines = 1).apply {
        background = rounded(p.color(night), dpf(CHIP_RADIUS))
        setPadding(dp(5), dp(1), dp(5), dp(1))
    }

/**
 * The portal chip: monogram and nickname in the portal color. It is the one component that names a portal on a card, a row, a
 * sheet and the alarm page. The nickname is always written: the color is never the only cue. [dashed]: the portal is down.
 */
fun Context.portalChip(p: PortalIdentity, night: Boolean = darkTheme(), dashed: Boolean = false, isNew: Boolean = false): LinearLayout =
    lin(vertical = false).apply {
        val c = p.color(night)
        background = tint(withAlpha(c, 0x21), withAlpha(c, 0x73), 6f, dashed)
        setPadding(dp(2), dp(1), dp(7), dp(1))
        addView(monogram(p, night, 18))
        addView(label(VisibleText.escapeLine(p.nickname), SP_S, c, BOLD, lines = 1), lp(WRAP, WRAP).margins(this@portalChip, l = 5))
        if (isNew) addView(newMark(p, night), lp(WRAP, WRAP).margins(this@portalChip, l = 6))
        contentDescription = p.nickname + (if (isNew) ", " + getString(R.string.portal_new) else "")
    }

/** The chip of a [HomePortal]: dashed when the portal is down, with the "New" mark. */
fun Context.portalChip(p: HomePortal, night: Boolean = darkTheme()): LinearLayout =
    portalChip(p.identity, night, dashed = p.health.isDown, isNew = p.isNew)

/** One chip of the filter strip. */
private fun Context.filterChip(chip: FilterStrip.Chip, onTap: () -> Unit): View {
    val night = darkTheme()
    val p = chip.portal?.identity
    val accent = p?.color(night) ?: color(R.color.primary)
    return lin(vertical = false, padH = 0, padV = 0).apply {
        minimumHeight = dp(40)
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(if (p == null) 14 else 8), 0, dp(12), 0)
        background = if (chip.selected) tint(withAlpha(accent, 0x2E), withAlpha(accent, 0xB3), 20f, chip.dashed)
        else tint(color(R.color.field), color(R.color.glass_stroke), 20f, chip.dashed)
        isClickable = true
        isFocusable = true
        contentDescription = chip.spoken()
        setOnClickListener { onTap() }
        if (p != null) addView(monogram(p, night, 20), lp(WRAP, WRAP).margins(this@filterChip, r = 6))
        addView(label(
            p?.let { VisibleText.escapeLine(it.nickname) } ?: getString(R.string.filter_all), SP_M,
            if (chip.selected) color(R.color.text_strong) else color(R.color.text), MEDIUM, lines = 1,
        ))
        if (chip.portal?.isNew == true) addView(newMark(p!!, night), lp(WRAP, WRAP).margins(this@filterChip, l = 6))
        if (chip.dashed) {
            val tintColor = if (chip.portal?.health?.isDanger == true) color(R.color.danger) else color(R.color.warning)
            addView(statusDot(tintColor, 8), lp(dp(8), dp(8)).margins(this@filterChip, l = 6))
        }
        addView(mono(chip.count.toString(), SP_S, if (chip.selected) color(R.color.text) else color(R.color.text_muted)),
            lp(WRAP, WRAP).margins(this@filterChip, l = 6))
    }
}

/** The filter strip: All, then one chip per portal. It scrolls sideways when the chips are wider than the screen. */
fun Context.filterStrip(chips: List<FilterStrip.Chip>, onTap: (FilterStrip.Chip) -> Unit): View {
    val row = lin(vertical = false, padH = 10, padV = 4)
    chips.forEachIndexed { i, chip ->
        row.addView(filterChip(chip) { onTap(chip) }, lp(WRAP, WRAP).margins(this, l = if (i == 0) 0 else 8))
    }
    return HorizontalScrollView(this).apply {
        isHorizontalScrollBarEnabled = false
        overScrollMode = View.OVER_SCROLL_NEVER
        contentDescription = getString(R.string.filter_strip_description)
        addView(row)
    }
}

/** The top bar: one dot per portal in its color, hollow when the portal is down. It replaces the single connection dot. */
fun Context.portalDots(portals: List<HomePortal>): View {
    val night = darkTheme()
    val row = lin(vertical = false).apply {
        gravity = Gravity.CENTER_VERTICAL
        contentDescription = FilterStrip.dotsSpoken(portals)
    }
    FilterStrip.dots(portals).forEachIndexed { i, dot ->
        val c = dot.identity.color(night)
        val d = View(this).apply {
            background = GradientDrawable().apply {
                shape = GradientDrawable.OVAL
                if (dot.hollow) setStroke(dp(2), c) else setColor(c)
            }
        }
        row.addView(d, lp(dp(10), dp(10)).margins(this, l = if (i == 0) 0 else 7))
    }
    return row
}

/** A banner of the home list: a bold title, a line of text, and an optional action. Never a notification. */
fun Context.statusBanner(b: OfflineSummary.Banner, onAction: () -> Unit): View {
    val danger = b.severity == OfflineSummary.Severity.DANGER
    val fg = color(if (danger) R.color.danger else R.color.warning)
    val bg = color(if (danger) R.color.danger_soft else R.color.warning_soft)
    val text = SpannableStringBuilder(b.title).apply {
        setSpan(StyleSpan(android.graphics.Typeface.BOLD), 0, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        append('\n').append(b.text)
    }
    if (b.action != null) return banner(text, b.action, fg, bg, R.drawable.ic_warning, onAction)
    return lin(vertical = false).apply {
        background = rounded(bg, dpf(CARD_RADIUS), fg and 0x4DFFFFFF, dp(1))
        addView(View(this@statusBanner).apply { setBackgroundColor(fg) }, lp(dp(3), MATCH))
        addView(icon(R.drawable.ic_warning, fg, 16), lp(dp(16), dp(16)).margins(this@statusBanner, l = 10, r = 8))
        addView(label(text, SP_S, color(R.color.text), lines = 4).apply { setPadding(0, dp(9), dp(12), dp(9)) }, lp(0, WRAP, 1f))
    }
}

/** The state colors of the last line of a row of a portal that is down. */
fun Context.reasonColor(health: PortalHealth): Int = color(if (health.isDanger) R.color.danger else R.color.warning)
