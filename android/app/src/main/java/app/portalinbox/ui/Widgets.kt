package app.portalinbox.ui

import android.app.Activity
import android.app.Dialog
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RadialGradient
import android.graphics.RenderEffect
import android.graphics.Shader
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import app.portalinbox.R
import kotlin.math.abs

/**
 * Page backdrop: graphite (or pale grey) with a single soft blue light in the top
 * corner, blurred on Android 12+ so the drawer and sheets read as frosted.
 */
class Backdrop(context: Context) : View(context) {
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val night = (resources.configuration.uiMode and android.content.res.Configuration.UI_MODE_NIGHT_MASK) ==
        android.content.res.Configuration.UI_MODE_NIGHT_YES
    private val blobs = listOf(
        Triple(R.color.backdrop_glow, floatArrayOf(0.1f, 0.04f), 0.95f),
    )

    init {
        setBackgroundColor(context.color(R.color.bg_page))
        if (Build.VERSION.SDK_INT >= 31) {
            setRenderEffect(RenderEffect.createBlurEffect(context.dpf(40f), context.dpf(40f), Shader.TileMode.CLAMP))
        }
    }

    private var circles: List<Triple<Float, Float, Float>> = emptyList()
    private var shaders: List<Shader> = emptyList()

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        val alpha = if (night) 0x44 else 0x55
        circles = blobs.map { (_, at, size) -> Triple(at[0] * w, at[1] * h, size * w) }
        shaders = blobs.mapIndexed { i, (res, _, _) ->
            val c = context.color(res) and 0x00FFFFFF
            val (cx, cy, r) = circles[i]
            RadialGradient(cx, cy, r, intArrayOf(c or (alpha shl 24), c or ((alpha / 3) shl 24), c),
                floatArrayOf(0f, 0.5f, 1f), Shader.TileMode.CLAMP)
        }
    }

    override fun onDraw(canvas: Canvas) {
        for (i in circles.indices) {
            val (cx, cy, r) = circles[i]
            paint.shader = shaders[i]
            canvas.drawCircle(cx, cy, r, paint)
        }
    }
}

/** Wraps a screen's content above the coloured backdrop. */
fun Context.withBackdrop(content: View): FrameLayout = FrameLayout(this).apply {
    addView(Backdrop(this@withBackdrop), FrameLayout.LayoutParams(MATCH, MATCH))
    addView(content, FrameLayout.LayoutParams(MATCH, MATCH))
}

/** Frosts `view` (Android 12+), e.g. the screen behind an open drawer or sheet. */
fun View.frost(on: Boolean) {
    if (Build.VERSION.SDK_INT < 31) return
    setRenderEffect(if (on) RenderEffect.createBlurEffect(context.dpf(9f), context.dpf(9f), Shader.TileMode.CLAMP) else null)
}

/**
 * Compact segmented control (sentence case, UI font). Each option is a radio for
 * TalkBack; [describe] gives its spoken label, e.g. "Ops: Ring".
 */
fun Context.segmented(
    options: List<String>,
    selected: Int,
    accent: (Int) -> Pair<Int, Int> = { color(R.color.primary) to color(R.color.primary_soft) },
    describe: ((Int) -> String)? = null,
    onSelect: (Int) -> Unit,
): LinearLayout = lin(vertical = false).apply {
    background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
    setPadding(dp(2), dp(2), dp(2), dp(2))
    options.forEachIndexed { i, text ->
        val on = i == selected
        val (fg, bg) = accent(i)
        addView(label(text, SP_S, if (on) fg else color(R.color.text_muted), if (on) BOLD else MEDIUM, lines = 1).apply {
            gravity = Gravity.CENTER
            minHeight = dp(44)
            setPadding(dp(4), 0, dp(4), 0)
            background = if (on) rounded(bg, dpf(CONTROL_RADIUS - 2), fg and 0x59FFFFFF, dp(1)) else pressable(0x00000000, dpf(CONTROL_RADIUS - 2))
            isClickable = true
            isFocusable = true
            contentDescription = describe?.invoke(i) ?: text
            if (Build.VERSION.SDK_INT >= 30) stateDescription = getString(if (on) R.string.a11y_selected else R.string.a11y_not_selected)
            accessibilityDelegate = radioDelegate(on)
            setOnClickListener { if (i != selected) onSelect(i) }
        }, lp(0, WRAP, 1f))
    }
}

/** Announces a view as a radio button, checked or not. */
private fun radioDelegate(checked: Boolean) = object : View.AccessibilityDelegate() {
    override fun onInitializeAccessibilityNodeInfo(host: View, info: android.view.accessibility.AccessibilityNodeInfo) {
        super.onInitializeAccessibilityNodeInfo(host, info)
        info.className = android.widget.RadioButton::class.java.name
        info.isCheckable = true
        @Suppress("DEPRECATION")
        info.isChecked = checked
    }
}

/** Compact panel row: title, optional subtitle, trailing view. */
fun Context.row(title: String, subtitle: String? = null, trailing: View? = null, onClick: (() -> Unit)? = null): LinearLayout =
    lin(vertical = false, padH = 14, padV = 10).apply {
        minimumHeight = dp(44)
        val texts = lin()
        texts.addView(label(title, SP_M, color(R.color.text), MEDIUM))
        subtitle?.let { texts.addView(label(it, SP_S, color(R.color.text_muted)), lp().margins(this@row, t = 2)) }
        addView(texts, lp(0, WRAP, 1f))
        trailing?.let { addView(it, lp(WRAP, WRAP).margins(this@row, l = 10)) }
        if (onClick != null) {
            background = rowRipple()
            isClickable = true
            setOnClickListener { onClick() }
        }
    }

/** Key / value row: muted key, right-aligned monospace value. */
fun Context.kv(key: String, value: String, valueColor: Int = color(R.color.text), lines: Int = 1, valueMono: Boolean = true): LinearLayout =
    lin(vertical = false, padH = 14, padV = 0).apply {
        minimumHeight = dp(32)
        setPadding(dp(14), dp(7), dp(14), dp(7))
        addView(label(key, SP_S, color(R.color.text_muted)), lp(WRAP, WRAP))
        val v = if (valueMono) mono(value, SP_S, valueColor, lines = lines) else label(value, SP_S, valueColor, MEDIUM, lines = lines)
        addView(v.apply { gravity = Gravity.END }, lp(0, WRAP, 1f).margins(this@kv, l = 12))
    }

/** Round tinted container with an icon in the middle. */
fun Context.iconBadge(res: Int, fg: Int, bg: Int, size: Int = 32): FrameLayout = FrameLayout(this).apply {
    background = rounded(bg, dpf(size * 0.3f), fg and 0x33FFFFFF, dp(1))
    addView(icon(res, fg, size / 2), FrameLayout.LayoutParams(dp(size / 2), dp(size / 2), Gravity.CENTER))
    layoutParams = LinearLayout.LayoutParams(dp(size), dp(size))
}

/** Thin strip with a coloured edge and a trailing action (sentence case). */
fun Context.banner(
    text: CharSequence,
    action: String,
    fg: Int,
    bg: Int,
    iconRes: Int = R.drawable.ic_warning,
    onAction: () -> Unit,
): LinearLayout =
    lin(vertical = false, padH = 0, padV = 0).apply {
        background = rounded(bg, dpf(CARD_RADIUS), fg and 0x4DFFFFFF, dp(1))
        clipToOutline = true
        outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
        addView(View(this@banner).apply { setBackgroundColor(fg) }, lp(dp(3), MATCH))
        addView(icon(iconRes, fg, 16), lp(dp(16), dp(16)).margins(this@banner, l = 10, r = 8))
        addView(label(text, SP_S, color(R.color.text), lines = 3).apply { setPadding(0, dp(9), 0, dp(9)) }, lp(0, WRAP, 1f))
        addView(label(action, SP_M, fg, MEDIUM).apply {
            gravity = Gravity.CENTER
            minHeight = dp(48)
            setPadding(dp(12), 0, dp(12), 0)
            isClickable = true
            isFocusable = true
            background = rowRipple()
            setOnClickListener { onAction() }
        })
    }

/**
 * Bottom snackbar inside [host] (a FrameLayout covering the screen): one line of
 * text and an action, gone after [durationMs] or once the action is used. Showing
 * a new one replaces the previous one.
 */
fun FrameLayout.snackbar(text: String, action: String? = null, durationMs: Long = 5_000, onAction: () -> Unit = {}) {
    val c = context
    findViewWithTag<View>(SNACKBAR_TAG)?.let { removeView(it) }
    val bar = c.lin(vertical = false).apply {
        tag = SNACKBAR_TAG
        background = rounded(c.color(R.color.snackbar), c.dpf(CARD_RADIUS))
        elevation = c.dpf(6f)
        minimumHeight = c.dp(48)
        setPadding(c.dp(16), 0, c.dp(if (action != null) 4 else 16), 0)
        accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
    }
    val dismiss = Runnable {
        bar.animate().alpha(0f).translationY(c.dpf(16f)).setDuration(150).withEndAction { removeView(bar) }.start()
    }
    bar.addView(c.label(text, SP_M, c.color(R.color.on_snackbar), lines = 2), lp(0, WRAP, 1f))
    if (action != null) bar.addView(c.label(action, SP_M, c.color(R.color.snackbar_action), BOLD).apply {
        gravity = Gravity.CENTER
        minHeight = c.dp(48)
        setPadding(c.dp(14), 0, c.dp(14), 0)
        isClickable = true
        isFocusable = true
        background = c.pressable(0x00000000, c.dpf(CONTROL_RADIUS))
        setOnClickListener {
            bar.removeCallbacks(dismiss)
            removeView(bar)
            onAction()
        }
    })
    val bottom = bottomInset() + c.dp(12)
    addView(bar, FrameLayout.LayoutParams(MATCH, WRAP, Gravity.BOTTOM).apply {
        setMargins(c.dp(10), 0, c.dp(10), bottom)
    })
    bar.alpha = 0f
    bar.translationY = c.dpf(16f)
    bar.animate().alpha(1f).translationY(0f).setDuration(180).start()
    bar.postDelayed(dismiss, durationMs)
}

private const val SNACKBAR_TAG = "inbox.snackbar"

/** Bottom system bar (or keyboard) inset currently applied to this view's window. */
@Suppress("DEPRECATION")
private fun View.bottomInset(): Int {
    val insets = rootWindowInsets ?: return 0
    return if (Build.VERSION.SDK_INT >= 30) {
        insets.getInsets(android.view.WindowInsets.Type.systemBars() or android.view.WindowInsets.Type.ime()).bottom
    } else {
        insets.systemWindowInsetBottom
    }
}

/** Bottom sheet on a plain Dialog: frosted glass, blurred backdrop on Android 12+. */
fun Activity.bottomSheet(build: (LinearLayout, Dialog) -> Unit): Dialog {
    val d = Dialog(this, R.style.Theme_Inbox_Sheet)
    val sheet = lin(padH = 16, padV = 10).apply {
        background = GradientDrawable().apply {
            setColor(color(R.color.glass_strong))
            val r = dpf(22f)
            cornerRadii = floatArrayOf(r, r, r, r, 0f, 0f, 0f, 0f)
            setStroke(dp(1), color(R.color.glass_stroke))
        }
        addView(View(this@bottomSheet).apply {
            background = rounded(color(R.color.text_subtle), dpf(2f))
            alpha = 0.6f
        }, lp(dp(36), dp(4)).apply { gravity = Gravity.CENTER_HORIZONTAL; bottomMargin = dp(12) })
    }
    build(sheet, d)
    d.setContentView(scroll(sheet))
    val content = window.decorView.findViewById<View>(android.R.id.content)
    d.window?.apply {
        setGravity(Gravity.BOTTOM)
        setLayout(WindowManager.LayoutParams.MATCH_PARENT, WindowManager.LayoutParams.WRAP_CONTENT)
        if (Build.VERSION.SDK_INT >= 31 && windowManager.isCrossWindowBlurEnabled) {
            addFlags(WindowManager.LayoutParams.FLAG_BLUR_BEHIND)
            attributes = attributes.apply { blurBehindRadius = dp(24) }
            setBackgroundBlurRadius(dp(30))
            setDimAmount(0.25f)
        } else {
            setDimAmount(0.45f)
        }
    }
    // Same-window frost as a fallback (and on top of cross-window blur).
    content.frost(true)
    d.setOnDismissListener { content.frost(false) }
    d.setCanceledOnTouchOutside(true)
    d.show()
    return d
}

/**
 * Navigation drawer panel (frosted glass): slides in from the left over a scrim;
 * dragging it to the left or tapping the scrim closes it. `behind` is frosted while
 * the drawer is open.
 */
class Drawer(context: Context, private val scrim: View, private val behind: View) : LinearLayout(context) {
    var isOpen = false
        private set
    /** Called after each open / close, e.g. to (un)register the Back callback. */
    var onToggle: (() -> Unit)? = null
    private var startX = 0f
    private var dragging = false
    private val slop = ViewConfiguration.get(context).scaledTouchSlop

    init {
        orientation = VERTICAL
        background = GradientDrawable().apply {
            setColor(context.color(R.color.glass_strong))
            val r = context.dpf(22f)
            cornerRadii = floatArrayOf(0f, 0f, r, r, r, r, 0f, 0f)
            setStroke(context.dp(1), context.color(R.color.glass_stroke))
        }
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
        behind.frost(true)
        scrim.animate().alpha(1f).setDuration(200).start()
        animate().translationX(0f).setDuration(220).start()
        onToggle?.invoke()
    }

    fun close() {
        isOpen = false
        behind.frost(false)
        scrim.animate().alpha(0f).setDuration(200).withEndAction { scrim.visibility = GONE }.start()
        animate().translationX(-width.toFloat()).setDuration(200).withEndAction {
            if (!isOpen) visibility = INVISIBLE
        }.start()
        onToggle?.invoke()
    }

    override fun onInterceptTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> { startX = e.rawX; dragging = false }
            MotionEvent.ACTION_MOVE -> if (startX - e.rawX > slop) { dragging = true; return true }
        }
        return false
    }

    // Dragging the panel is a gesture, not a click: nothing for performClick to do.
    @android.annotation.SuppressLint("ClickableViewAccessibility")
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

/** Width of the drawer: 78 % of the screen, at most 300 dp. */
fun Context.drawerWidth(): Int = minOf((resources.displayMetrics.widthPixels * 0.78f).toInt(), dp(300))

fun TextView.asLink(): TextView = apply {
    setTextColor(context.color(R.color.primary))
    typeface = MEDIUM
    isClickable = true
}

fun ViewGroup.addAll(vararg views: View) = views.forEach { addView(it) }

/**
 * Status light: a small dot whose halo breathes every two seconds (all servers
 * connected). Static when [pulse] is false.
 */
class PulseDot(context: Context, private val tint: Int, private val pulse: Boolean = true) : View(context) {
    private val core = android.graphics.Paint(android.graphics.Paint.ANTI_ALIAS_FLAG).apply { color = tint }
    private val halo = android.graphics.Paint(android.graphics.Paint.ANTI_ALIAS_FLAG).apply { color = tint }
    private var phase = 0f
    private val animator = android.animation.ValueAnimator.ofFloat(0f, 1f).apply {
        duration = 2000
        repeatCount = android.animation.ValueAnimator.INFINITE
        interpolator = android.view.animation.AccelerateDecelerateInterpolator()
        addUpdateListener {
            phase = it.animatedValue as Float
            invalidate()
        }
    }

    /** 22 dp unless the parent imposes a size. */
    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val size = context.dp(22)
        setMeasuredDimension(resolveSize(size, widthMeasureSpec), resolveSize(size, heightMeasureSpec))
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        if (pulse) animator.start()
    }

    override fun onDetachedFromWindow() {
        animator.cancel()
        super.onDetachedFromWindow()
    }

    override fun onDraw(canvas: android.graphics.Canvas) {
        val cx = width / 2f
        val cy = height / 2f
        val r = minOf(width, height) * 0.18f
        if (pulse) {
            // Halo grows from the core to the bounds while fading out, then repeats.
            val max = minOf(width, height) / 2f
            halo.alpha = ((1f - phase) * 110).toInt()
            canvas.drawCircle(cx, cy, r + (max - r) * phase, halo)
            // The core itself glows a little with the breath.
            core.alpha = (200 + 55 * kotlin.math.sin(phase * Math.PI).toFloat()).toInt()
        }
        canvas.drawCircle(cx, cy, r, core)
    }
}
