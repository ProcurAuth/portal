package app.inbox.ui

import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.graphics.drawable.Drawable
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.LayerDrawable
import android.graphics.drawable.RippleDrawable
import android.text.TextUtils
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import app.inbox.R
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/*
 * Programmatic view toolkit (the app ships without AndroidX nor XML layouts).
 * Visual language, "industrial glass": dense rows like an observability event
 * viewer (monospace data, uppercase micro-labels, hairlines, severity bars) laid
 * on frosted translucent panels over a softly coloured backdrop.
 */

fun Context.dp(v: Int): Int = (v * resources.displayMetrics.density + 0.5f).toInt()
fun Context.dpf(v: Float): Float = v * resources.displayMetrics.density
fun Context.color(id: Int): Int = getColor(id)

val MATCH = ViewGroup.LayoutParams.MATCH_PARENT
val WRAP = ViewGroup.LayoutParams.WRAP_CONTENT

val MEDIUM: Typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
val BOLD: Typeface = Typeface.create("sans-serif", Typeface.BOLD)
val REGULAR: Typeface = Typeface.create("sans-serif", Typeface.NORMAL)
val LIGHT: Typeface = Typeface.create("sans-serif-light", Typeface.NORMAL)
val MONO: Typeface = Typeface.MONOSPACE
val MONO_BOLD: Typeface = Typeface.create(Typeface.MONOSPACE, Typeface.BOLD)

/** Panel radius (continuous-looking large corners) and compact control radius. */
const val PANEL_RADIUS = 18f
const val CONTROL_RADIUS = 8f
const val CHIP_RADIUS = 4f

fun rounded(fill: Int, radius: Float, stroke: Int? = null, strokeWidth: Int = 0): GradientDrawable =
    GradientDrawable().apply {
        setColor(fill)
        cornerRadius = radius
        if (stroke != null) setStroke(strokeWidth, stroke)
    }

/**
 * Frosted glass: translucent fill, 1 px edge, and a faint highlight on the upper
 * part like light caught by the top of a pane.
 */
fun Context.glass(radius: Float = dpf(PANEL_RADIUS), fill: Int = color(R.color.glass)): Drawable {
    val base = rounded(fill, radius, color(R.color.glass_stroke), dp(1))
    val sheen = GradientDrawable(
        GradientDrawable.Orientation.TOP_BOTTOM,
        intArrayOf(color(R.color.glass_highlight) and 0x55FFFFFF, 0x00FFFFFF),
    ).apply { cornerRadius = radius }
    return LayerDrawable(arrayOf(base, sheen)).apply {
        setLayerInset(1, dp(1), dp(1), dp(1), 0)
        setLayerGravity(1, Gravity.TOP or Gravity.FILL_HORIZONTAL)
        setLayerHeight(1, dp(28))
    }
}

/** Background with a pressed ripple clipped to the same shape. */
fun Context.pressable(fill: Int, radius: Float, stroke: Int? = null): Drawable {
    val shape = rounded(fill, radius, stroke, if (stroke != null) dp(1) else 0)
    val mask = rounded(0xFFFFFFFF.toInt(), radius)
    return RippleDrawable(ColorStateList.valueOf(color(R.color.bg_hover)), shape, mask)
}

/** Transparent, rectangular ripple for rows inside a panel. */
fun Context.rowRipple(): Drawable =
    RippleDrawable(ColorStateList.valueOf(color(R.color.bg_hover)), null, rounded(0xFFFFFFFF.toInt(), 0f))

fun Context.lin(vertical: Boolean = true, padH: Int = 0, padV: Int = 0): LinearLayout = LinearLayout(this).apply {
    orientation = if (vertical) LinearLayout.VERTICAL else LinearLayout.HORIZONTAL
    if (!vertical) gravity = Gravity.CENTER_VERTICAL
    setPadding(dp(padH), dp(padV), dp(padH), dp(padV))
}

fun Context.label(
    value: CharSequence,
    size: Float = 14f,
    color: Int = color(R.color.text),
    face: Typeface = REGULAR,
    lines: Int = 0,
): TextView = TextView(this).apply {
    text = value
    textSize = size
    typeface = face
    setTextColor(color)
    includeFontPadding = false
    setLineSpacing(dpf(1.5f), 1f)
    if (lines > 0) {
        maxLines = lines
        ellipsize = TextUtils.TruncateAt.END
    }
}

/** Monospace data text with tabular figures. */
fun Context.mono(value: CharSequence, size: Float = 12f, color: Int = color(R.color.text), bold: Boolean = false, lines: Int = 1): TextView =
    label(value, size, color, if (bold) MONO_BOLD else MONO, lines).apply {
        fontFeatureSettings = "tnum"
    }

fun Context.icon(res: Int, tint: Int, size: Int = 20): ImageView = ImageView(this).apply {
    setImageResource(res)
    imageTintList = ColorStateList.valueOf(tint)
    layoutParams = LinearLayout.LayoutParams(dp(size), dp(size))
}

/** A 40 dp touch target holding an icon. */
fun Context.iconButton(res: Int, description: String, tint: Int = color(R.color.text), size: Int = 20, onClick: () -> Unit): FrameLayout =
    FrameLayout(this).apply {
        contentDescription = description
        background = RippleDrawable(ColorStateList.valueOf(color(R.color.bg_hover)), null, null)
        isClickable = true
        setOnClickListener { onClick() }
        addView(icon(res, tint, size), FrameLayout.LayoutParams(dp(size), dp(size), Gravity.CENTER))
        layoutParams = LinearLayout.LayoutParams(dp(40), dp(40))
    }

/** Glass panel holding compact rows. */
fun Context.panel(padH: Int = 0, padV: Int = 0): LinearLayout = lin(padH = padH, padV = padV).apply {
    background = glass()
    clipToOutline = true
    outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
}

/** Square-ish monospace tag: "ALARME", "SONNERIE", "EN LIGNE"… */
fun Context.chip(value: String, fg: Int, bg: Int, dot: Boolean = false): TextView =
    mono(value.uppercase(), 10f, fg, bold = true).apply {
        letterSpacing = 0.06f
        background = rounded(bg, dpf(CHIP_RADIUS), fg and 0x40FFFFFF, dp(1))
        setPadding(dp(6), dp(3), dp(6), dp(3))
        gravity = Gravity.CENTER_VERTICAL
        if (dot) {
            val d = rounded(fg, dpf(3f)).apply { setSize(dp(6), dp(6)) }
            setCompoundDrawablesRelativeWithIntrinsicBounds(d, null, null, null)
            compoundDrawablePadding = dp(5)
        }
    }

/** Kept for compatibility with older call sites: a chip without uppercase forcing. */
fun Context.pill(value: String, fg: Int, bg: Int, dot: Boolean = false): TextView = chip(value, fg, bg, dot)

fun Context.statusDot(fg: Int, size: Int = 7): View = View(this).apply {
    background = rounded(fg, dpf(size / 2f))
    layoutParams = LinearLayout.LayoutParams(dp(size), dp(size))
}

enum class ButtonKind { PRIMARY, SECONDARY, DANGER }

fun Context.button(value: String, kind: ButtonKind = ButtonKind.PRIMARY, onClick: () -> Unit): TextView =
    label(value, 14f, face = MEDIUM).apply {
        gravity = Gravity.CENTER
        minHeight = dp(40)
        setPadding(dp(14), 0, dp(14), 0)
        isClickable = true
        isFocusable = true
        when (kind) {
            ButtonKind.PRIMARY -> {
                setTextColor(color(R.color.on_primary))
                background = pressable(color(R.color.primary), dpf(CONTROL_RADIUS))
            }
            ButtonKind.SECONDARY -> {
                setTextColor(color(R.color.text))
                background = pressable(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke))
            }
            ButtonKind.DANGER -> {
                setTextColor(color(R.color.danger))
                background = pressable(color(R.color.danger_soft), dpf(CONTROL_RADIUS), color(R.color.danger) and 0x40FFFFFF)
            }
        }
        setOnClickListener { onClick() }
    }

/** Uppercase micro-label above a panel, with an optional right-aligned mono value. */
fun Context.sectionLabel(value: String, trailing: String? = null): View {
    val row = lin(vertical = false, padH = 6, padV = 0).apply { setPadding(dp(6), dp(14), dp(6), dp(6)) }
    row.addView(label(value.uppercase(), 11f, color(R.color.text_muted), MEDIUM).apply { letterSpacing = 0.12f }, lp(0, WRAP, 1f))
    trailing?.let { row.addView(mono(it, 11f, color(R.color.text_muted))) }
    return row
}

fun Context.divider(insetStart: Int = 0): View = View(this).apply {
    setBackgroundColor(color(R.color.hairline))
    layoutParams = LinearLayout.LayoutParams(MATCH, 1).apply { marginStart = dp(insetStart) }
}

fun Context.scroll(content: View): ScrollView = ScrollView(this).apply {
    isFillViewport = true
    clipToPadding = false
    isVerticalScrollBarEnabled = false
    overScrollMode = View.OVER_SCROLL_IF_CONTENT_SCROLLS
    addView(content, ViewGroup.LayoutParams(MATCH, WRAP))
}

fun lp(w: Int = MATCH, h: Int = WRAP, weight: Float = 0f): LinearLayout.LayoutParams =
    LinearLayout.LayoutParams(w, h, weight)

fun LinearLayout.LayoutParams.margins(c: Context, l: Int = 0, t: Int = 0, r: Int = 0, b: Int = 0) = apply {
    setMargins(c.dp(l), c.dp(t), c.dp(r), c.dp(b))
}

/**
 * Compact app bar (48 dp) drawn directly on the page background: navigation icon
 * on the left, optional trailing view (status) on the right. No title and no bar
 * background: the screen context is the content's section label.
 */
fun Context.topBar(navRes: Int, navDescription: String, onNav: () -> Unit, trailing: View? = null): LinearLayout {
    val bar = lin(vertical = false, padH = 4, padV = 0).apply {
        minimumHeight = dp(48)
        addView(iconButton(navRes, navDescription, color(R.color.text_strong), 20, onNav))
        addView(View(this@topBar), lp(0, 1, 1f))
        if (trailing != null) addView(trailing, lp(WRAP, WRAP).margins(this@topBar, r = 10))
    }
    return lin().apply { addView(bar, lp(MATCH, dp(48))) }
}

// ---------------------------------------------------------------- semantics

data class ActionStyle(val icon: Int, val fg: Int, val bg: Int, val label: String, val code: String)

fun Context.actionStyle(action: String?): ActionStyle = when (action) {
    "alarm" -> ActionStyle(R.drawable.ic_alarm, color(R.color.danger), color(R.color.danger_soft), getString(R.string.action_alarm), "ALARME")
    "ring" -> ActionStyle(R.drawable.ic_bell_ring, color(R.color.warning), color(R.color.warning_soft), getString(R.string.action_ring), "SONNERIE")
    "notify" -> ActionStyle(R.drawable.ic_bell, color(R.color.info), color(R.color.info_soft), getString(R.string.action_notify), "NOTIF")
    else -> ActionStyle(R.drawable.ic_bell_outline, color(R.color.text_muted), color(R.color.bg_hover), getString(R.string.action_off), "AUCUNE")
}

/** "à l'instant", "il y a 5 min", "il y a 3 h", "hier", "il y a 4 j". */
fun relativeTime(thenMs: Long, nowMs: Long = System.currentTimeMillis()): String {
    val s = ((nowMs - thenMs) / 1000).coerceAtLeast(0)
    return when {
        s < 45 -> "à l'instant"
        s < 3_600 -> "il y a ${(s + 30) / 60} min"
        s < 86_400 -> "il y a ${s / 3_600} h"
        s < 172_800 -> "hier"
        else -> "il y a ${s / 86_400} j"
    }
}

/** Compact age for dense rows: "<1 min", "4 min", "3 h", "2 j". */
fun shortAge(thenMs: Long, nowMs: Long = System.currentTimeMillis()): String {
    val s = ((nowMs - thenMs) / 1000).coerceAtLeast(0)
    return when {
        s < 45 -> "<1 min"
        s < 3_600 -> "${(s + 30) / 60} min"
        s < 86_400 -> "${s / 3_600} h"
        else -> "${s / 86_400} j"
    }
}

/** "depuis 3 min" etc. */
fun formatSince(ms: Long): String {
    val s = ms / 1000
    return when {
        s < 60 -> "depuis ${s} s"
        s < 3600 -> "depuis ${s / 60} min"
        s < 86400 -> "depuis ${s / 3600} h"
        else -> "depuis ${s / 86400} j"
    }
}

/** Human duration for a lateness: "3 min", "2 h", "1 j". */
fun formatDuration(ms: Long): String {
    val s = ms / 1000
    return when {
        s < 3600 -> "${(s / 60).coerceAtLeast(1)} min"
        s < 86400 -> "${s / 3600} h"
        else -> "${s / 86400} j"
    }
}

/** Display window label: "1 h", "24 h", "7 j". */
fun windowLabel(ms: Long): String {
    val h = ms / 3_600_000
    return if (h >= 168 && h % 24 == 0L) "${h / 24} j" else "$h h"
}

/** The app is French-only: dates follow, whatever the phone locale. */
fun formatTime(ms: Long): String = SimpleDateFormat("dd/MM/yyyy HH:mm:ss", Locale.FRANCE).format(Date(ms))

/** "14:02:31". */
fun hms(ms: Long): String = SimpleDateFormat("HH:mm:ss", Locale.FRANCE).format(Date(ms))

/** "14:02:31" for today, "12/09 14:02" for older entries. */
fun clockTime(ms: Long, nowMs: Long = System.currentTimeMillis()): String {
    val sameDay = SimpleDateFormat("yyyyMMdd", Locale.FRANCE).let { it.format(Date(ms)) == it.format(Date(nowMs)) }
    return SimpleDateFormat(if (sameDay) "HH:mm:ss" else "dd/MM HH:mm", Locale.FRANCE).format(Date(ms))
}
