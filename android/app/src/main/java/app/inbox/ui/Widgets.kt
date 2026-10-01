package app.inbox.ui

import android.app.Activity
import android.app.Dialog
import android.content.Context
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import app.inbox.R
import kotlin.math.abs

/** Segmented control. `accent` gives the selected colours per option (fg, bg). */
fun Context.segmented(
    options: List<String>,
    selected: Int,
    accent: (Int) -> Pair<Int, Int> = { color(R.color.primary) to color(R.color.primary_soft) },
    onSelect: (Int) -> Unit,
): LinearLayout = lin(vertical = false).apply {
    background = rounded(color(R.color.bg_subtle), dpf(9f), color(R.color.border), dp(1))
    setPadding(dp(3), dp(3), dp(3), dp(3))
    options.forEachIndexed { i, text ->
        val on = i == selected
        val (fg, bg) = accent(i)
        addView(label(text, 13f, if (on) fg else color(R.color.text_muted), MEDIUM, lines = 1).apply {
            gravity = Gravity.CENTER
            minHeight = dp(36)
            setPadding(dp(4), 0, dp(4), 0)
            background = if (on) rounded(bg, dpf(7f), fg and 0x55FFFFFF, dp(1)) else pressable(0x00000000, dpf(7f))
            isClickable = true
            setOnClickListener { if (i != selected) onSelect(i) }
        }, lp(0, WRAP, 1f))
    }
}

/** A settings-style row: title, optional subtitle, and a trailing view. */
fun Context.row(title: String, subtitle: String? = null, trailing: View? = null, onClick: (() -> Unit)? = null): LinearLayout =
    lin(vertical = false, padH = 16, padV = 14).apply {
        val texts = lin()
        texts.addView(label(title, 15f, color(R.color.text), MEDIUM))
        subtitle?.let { texts.addView(label(it, 13f, color(R.color.text_muted)), lp().margins(this@row, t = 4)) }
        addView(texts, lp(0, WRAP, 1f))
        trailing?.let { addView(it, lp(WRAP, WRAP).margins(this@row, l = 12)) }
        if (onClick != null) {
            background = pressable(0x00000000, 0f)
            isClickable = true
            setOnClickListener { onClick() }
        }
    }

/** Round tinted container with an icon in the middle. */
fun Context.iconBadge(res: Int, fg: Int, bg: Int, size: Int = 40): FrameLayout = FrameLayout(this).apply {
    background = rounded(bg, dpf(size / 2f))
    addView(icon(res, fg, size / 2), FrameLayout.LayoutParams(dp(size / 2), dp(size / 2), Gravity.CENTER))
    layoutParams = LinearLayout.LayoutParams(dp(size), dp(size))
}

/** Small banner for warnings at the top of a screen. */
fun Context.banner(text: String, action: String, fg: Int, bg: Int, onAction: () -> Unit): LinearLayout =
    lin(vertical = false, padH = 14, padV = 12).apply {
        background = rounded(bg, dpf(12f), fg and 0x40FFFFFF, dp(1))
        addView(icon(R.drawable.ic_warning, fg, 20), lp(dp(20), dp(20)).margins(this@banner, r = 12))
        addView(label(text, 13f, color(R.color.text)), lp(0, WRAP, 1f))
        addView(label(action, 14f, fg, MEDIUM).apply {
            setPadding(dp(12), dp(8), dp(4), dp(8))
            isClickable = true
            setOnClickListener { onAction() }
        })
    }

/** Bottom sheet built on a plain Dialog. */
fun Activity.bottomSheet(build: (LinearLayout, Dialog) -> Unit): Dialog {
    val d = Dialog(this, R.style.Theme_Inbox_Sheet)
    val sheet = lin(padH = 20, padV = 12).apply {
        background = GradientDrawable().apply {
            setColor(color(R.color.bg_panel))
            val r = dpf(20f)
            cornerRadii = floatArrayOf(r, r, r, r, 0f, 0f, 0f, 0f)
        }
        addView(View(this@bottomSheet).apply {
            background = rounded(color(R.color.border_strong), dpf(2f))
        }, lp(dp(40), dp(4)).apply { gravity = Gravity.CENTER_HORIZONTAL; bottomMargin = dp(16) })
    }
    build(sheet, d)
    d.setContentView(scroll(sheet))
    d.window?.apply {
        setGravity(Gravity.BOTTOM)
        setLayout(WindowManager.LayoutParams.MATCH_PARENT, WindowManager.LayoutParams.WRAP_CONTENT)
    }
    d.setCanceledOnTouchOutside(true)
    d.show()
    return d
}

/**
 * Navigation drawer panel: slides in from the left over a scrim; dragging it to the
 * left or tapping the scrim closes it.
 */
class Drawer(context: Context, private val scrim: View) : LinearLayout(context) {
    var isOpen = false
        private set
    private var startX = 0f
    private var dragging = false
    private val slop = ViewConfiguration.get(context).scaledTouchSlop

    init {
        orientation = VERTICAL
        setBackgroundColor(context.color(R.color.bg_panel))
        elevation = context.dpf(16f)
        isClickable = true
        scrim.setBackgroundColor(context.color(R.color.scrim))
        scrim.alpha = 0f
        scrim.visibility = GONE
        scrim.setOnClickListener { close() }
        visibility = INVISIBLE
        post { if (!isOpen) translationX = -width.toFloat() }
    }

    fun open() {
        isOpen = true
        visibility = VISIBLE
        scrim.visibility = VISIBLE
        scrim.animate().alpha(1f).setDuration(200).start()
        animate().translationX(0f).setDuration(220).start()
    }

    fun close() {
        isOpen = false
        scrim.animate().alpha(0f).setDuration(200).withEndAction { scrim.visibility = GONE }.start()
        animate().translationX(-width.toFloat()).setDuration(200).withEndAction {
            // Hidden when closed so its elevation shadow does not bleed on the screen edge.
            if (!isOpen) visibility = INVISIBLE
        }.start()
    }

    override fun onInterceptTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> { startX = e.rawX; dragging = false }
            MotionEvent.ACTION_MOVE -> if (startX - e.rawX > slop) { dragging = true; return true }
        }
        return false
    }

    override fun onTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> startX = e.rawX
            MotionEvent.ACTION_MOVE -> {
                val dx = (e.rawX - startX).coerceAtMost(0f)
                if (abs(dx) > slop) dragging = true
                translationX = dx
                scrim.alpha = 1f + dx / width
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                if (dragging && translationX < -width / 3f) close() else open()
                dragging = false
            }
        }
        return true
    }
}

/** Width of the drawer: 82 % of the screen, at most 320 dp. */
fun Context.drawerWidth(): Int = minOf((resources.displayMetrics.widthPixels * 0.82f).toInt(), dp(320))

fun TextView.asLink(): TextView = apply {
    setTextColor(context.color(R.color.primary))
    typeface = MEDIUM
    isClickable = true
}

fun ViewGroup.addAll(vararg views: View) = views.forEach { addView(it) }
