package app.inbox.ui

import android.content.Context
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.text.DateFormat
import java.util.Date

/** Small programmatic view helpers: the app ships without AndroidX or XML layouts. */
fun Context.dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()

fun Context.column(padding: Int = 16): LinearLayout = LinearLayout(this).apply {
    orientation = LinearLayout.VERTICAL
    setPadding(dp(padding), dp(padding), dp(padding), dp(padding))
}

fun Context.scroll(content: View): ScrollView = ScrollView(this).apply {
    isFillViewport = true
    addView(content, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
}

fun Context.text(value: CharSequence, size: Float = 15f, bold: Boolean = false, color: Int? = null): TextView =
    TextView(this).apply {
        text = value
        textSize = size
        if (bold) setTypeface(typeface, Typeface.BOLD)
        color?.let { setTextColor(it) }
        setPadding(0, dp(4), 0, dp(4))
    }

fun Context.button(label: String, onClick: () -> Unit): Button = Button(this).apply {
    text = label
    isAllCaps = false
    setOnClickListener { onClick() }
}

fun Context.card(accent: Int? = null): LinearLayout = column(12).apply {
    background = GradientDrawable().apply {
        cornerRadius = dp(10).toFloat()
        setStroke(dp(1), accent ?: 0x33888888)
        setColor(0x0F888888)
    }
    layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT
    ).apply { setMargins(0, dp(6), 0, dp(6)) }
}

const val RED = 0xFFC62828.toInt()
const val GREEN = 0xFF2E7D32.toInt()
const val ORANGE = 0xFFEF6C00.toInt()
const val GREY = 0xFF757575.toInt()

fun formatAgo(ms: Long): String {
    val s = ms / 1000
    return when {
        s < 60 -> "depuis ${s}s"
        s < 3600 -> "depuis ${s / 60} min"
        s < 86400 -> "depuis ${s / 3600} h ${(s % 3600) / 60} min"
        else -> "depuis ${s / 86400} j"
    }
}

fun formatTime(ms: Long): String =
    DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.MEDIUM).format(Date(ms))

fun actionLabel(action: String?): String = when (action) {
    "alarm" -> "Alarme"
    "ring" -> "Sonnerie"
    "notify" -> "Notification"
    else -> "Désactivé"
}
