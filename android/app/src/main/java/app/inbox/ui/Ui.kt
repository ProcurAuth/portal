package app.inbox.ui

import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.graphics.drawable.Drawable
import android.graphics.drawable.GradientDrawable
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
import java.text.DateFormat
import java.util.Date
import java.util.Locale

/*
 * Programmatic view toolkit (the app ships without AndroidX nor XML layouts).
 * Visual language from S3-Browser: neutral panels with a 1 dp border, 12 dp
 * cards, 9 dp controls, system font, one blue accent and soft status colours.
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

fun rounded(fill: Int, radius: Float, stroke: Int? = null, strokeWidth: Int = 0): GradientDrawable =
    GradientDrawable().apply {
        setColor(fill)
        cornerRadius = radius
        if (stroke != null) setStroke(strokeWidth, stroke)
    }

/** Background with a pressed ripple clipped to the same shape. */
fun Context.pressable(fill: Int, radius: Float, stroke: Int? = null): Drawable {
    val shape = rounded(fill, radius, stroke, if (stroke != null) dp(1) else 0)
    val mask = rounded(0xFFFFFFFF.toInt(), radius)
    return RippleDrawable(ColorStateList.valueOf(color(R.color.bg_hover)), shape, mask)
}

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
    setLineSpacing(dpf(2f), 1f)
    if (lines > 0) {
        maxLines = lines
        ellipsize = TextUtils.TruncateAt.END
    }
}

fun Context.icon(res: Int, tint: Int, size: Int = 20): ImageView = ImageView(this).apply {
    setImageResource(res)
    imageTintList = ColorStateList.valueOf(tint)
    layoutParams = LinearLayout.LayoutParams(dp(size), dp(size))
}

/** A 40 dp round touch target holding an icon. */
fun Context.iconButton(res: Int, description: String, tint: Int = color(R.color.text), onClick: () -> Unit): FrameLayout =
    FrameLayout(this).apply {
        contentDescription = description
        background = RippleDrawable(ColorStateList.valueOf(color(R.color.bg_hover)), null, null)
        isClickable = true
        setOnClickListener { onClick() }
        addView(icon(res, tint, 24), FrameLayout.LayoutParams(dp(24), dp(24), Gravity.CENTER))
        layoutParams = LinearLayout.LayoutParams(dp(44), dp(44))
    }

fun Context.card(padding: Int = 16): LinearLayout = lin(padH = padding, padV = padding).apply {
    background = rounded(color(R.color.bg_panel), dpf(12f), color(R.color.border), dp(1))
    elevation = dpf(1f)
    outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
}

fun Context.pill(value: String, fg: Int, bg: Int, dot: Boolean = false): TextView = label(value, 12f, fg, MEDIUM).apply {
    background = rounded(bg, dpf(999f))
    setPadding(dp(10), dp(5), dp(10), dp(5))
    gravity = Gravity.CENTER_VERTICAL
    if (dot) {
        val d = rounded(fg, dpf(4f)).apply { setSize(dp(7), dp(7)) }
        setCompoundDrawablesRelativeWithIntrinsicBounds(d, null, null, null)
        compoundDrawablePadding = dp(6)
    }
}

enum class ButtonKind { PRIMARY, SECONDARY, DANGER }

fun Context.button(value: String, kind: ButtonKind = ButtonKind.PRIMARY, onClick: () -> Unit): TextView =
    label(value, 15f, face = MEDIUM).apply {
        gravity = Gravity.CENTER
        minHeight = dp(46)
        setPadding(dp(16), 0, dp(16), 0)
        isClickable = true
        isFocusable = true
        when (kind) {
            ButtonKind.PRIMARY -> {
                setTextColor(color(R.color.on_primary))
                background = pressable(color(R.color.primary), dpf(9f))
            }
            ButtonKind.SECONDARY -> {
                setTextColor(color(R.color.text))
                background = pressable(color(R.color.bg_panel), dpf(9f), color(R.color.border))
            }
            ButtonKind.DANGER -> {
                setTextColor(color(R.color.danger))
                background = pressable(color(R.color.bg_panel), dpf(9f), color(R.color.border))
            }
        }
        setOnClickListener { onClick() }
    }

fun Context.sectionLabel(value: String): TextView =
    label(value.uppercase(), 12f, color(R.color.text_muted), MEDIUM).apply {
        letterSpacing = 0.06f
        setPadding(dp(4), dp(20), dp(4), dp(8))
    }

fun Context.divider(): View = View(this).apply {
    setBackgroundColor(color(R.color.border))
    layoutParams = LinearLayout.LayoutParams(MATCH, dp(1))
}

fun Context.scroll(content: View): ScrollView = ScrollView(this).apply {
    isFillViewport = true
    clipToPadding = false
    overScrollMode = View.OVER_SCROLL_IF_CONTENT_SCROLLS
    addView(content, ViewGroup.LayoutParams(MATCH, WRAP))
}

fun lp(w: Int = MATCH, h: Int = WRAP, weight: Float = 0f): LinearLayout.LayoutParams =
    LinearLayout.LayoutParams(w, h, weight)

fun LinearLayout.LayoutParams.margins(c: Context, l: Int = 0, t: Int = 0, r: Int = 0, b: Int = 0) = apply {
    setMargins(c.dp(l), c.dp(t), c.dp(r), c.dp(b))
}

/** App bar: navigation icon, title and an optional trailing view. */
fun Context.topBar(title: String, navRes: Int, navDescription: String, onNav: () -> Unit, trailing: View? = null): LinearLayout =
    lin(vertical = false, padH = 4, padV = 0).apply {
        minimumHeight = dp(56)
        setBackgroundColor(color(R.color.bg_page))
        addView(iconButton(navRes, navDescription, onClick = onNav))
        addView(label(title, 20f, color(R.color.text_strong), MEDIUM).apply {
            setPadding(dp(8), 0, 0, 0)
        }, lp(0, WRAP, 1f))
        if (trailing != null) {
            addView(trailing, lp(WRAP, WRAP).margins(this@topBar, r = 12))
        }
    }

// ---------------------------------------------------------------- semantics

data class ActionStyle(val icon: Int, val fg: Int, val bg: Int, val label: String)

fun Context.actionStyle(action: String?): ActionStyle = when (action) {
    "alarm" -> ActionStyle(R.drawable.ic_alarm, color(R.color.danger), color(R.color.danger_soft), getString(R.string.action_alarm))
    "ring" -> ActionStyle(R.drawable.ic_bell_ring, color(R.color.warning), color(R.color.warning_soft), getString(R.string.action_ring))
    "notify" -> ActionStyle(R.drawable.ic_bell, color(R.color.primary), color(R.color.primary_soft), getString(R.string.action_notify))
    else -> ActionStyle(R.drawable.ic_bell_outline, color(R.color.text_muted), color(R.color.bg_hover), getString(R.string.action_off))
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
fun formatTime(ms: Long): String =
    DateFormat.getDateTimeInstance(DateFormat.LONG, DateFormat.SHORT, Locale.FRANCE).format(Date(ms))
