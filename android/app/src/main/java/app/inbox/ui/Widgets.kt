package app.inbox.ui

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
import app.inbox.R
import kotlin.math.abs

/**
 * Softly coloured backdrop behind the glass: a few large radial blobs on the page
 * colour, blurred on Android 12+ so translucent panels read as frosted glass.
 */
class Backdrop(context: Context) : View(context) {
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val night = (resources.configuration.uiMode and android.content.res.Configuration.UI_MODE_NIGHT_MASK) ==
        android.content.res.Configuration.UI_MODE_NIGHT_YES
    private val blobs = listOf(
        Triple(R.color.blob_1, floatArrayOf(0.12f, 0.08f), 0.75f),
        Triple(R.color.blob_2, floatArrayOf(1.0f, 0.42f), 0.7f),
        Triple(R.color.blob_3, floatArrayOf(0.15f, 0.95f), 0.8f),
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
        val alpha = if (night) 0x55 else 0x66
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

/** Compact segmented control with monospace uppercase options. */
fun Context.segmented(
    options: List<String>,
    selected: Int,
    accent: (Int) -> Pair<Int, Int> = { color(R.color.primary) to color(R.color.primary_soft) },
    onSelect: (Int) -> Unit,
): LinearLayout = lin(vertical = false).apply {
    background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
    setPadding(dp(2), dp(2), dp(2), dp(2))
    options.forEachIndexed { i, text ->
        val on = i == selected
        val (fg, bg) = accent(i)
        addView(mono(text.uppercase(), 11f, if (on) fg else color(R.color.text_muted), bold = on).apply {
            letterSpacing = 0.04f
            gravity = Gravity.CENTER
            minHeight = dp(28)
            setPadding(dp(2), 0, dp(2), 0)
            background = if (on) rounded(bg, dpf(CONTROL_RADIUS - 2), fg and 0x59FFFFFF, dp(1)) else pressable(0x00000000, dpf(CONTROL_RADIUS - 2))
            isClickable = true
            setOnClickListener { if (i != selected) onSelect(i) }
        }, lp(0, WRAP, 1f))
    }
}

/** Compact panel row: title, optional subtitle, trailing view. */
fun Context.row(title: String, subtitle: String? = null, trailing: View? = null, onClick: (() -> Unit)? = null): LinearLayout =
    lin(vertical = false, padH = 14, padV = 10).apply {
        minimumHeight = dp(44)
        val texts = lin()
        texts.addView(label(title, 14f, color(R.color.text), MEDIUM))
        subtitle?.let { texts.addView(label(it, 11.5f, color(R.color.text_muted)), lp().margins(this@row, t = 2)) }
        addView(texts, lp(0, WRAP, 1f))
        trailing?.let { addView(it, lp(WRAP, WRAP).margins(this@row, l = 10)) }
        if (onClick != null) {
            background = rowRipple()
            isClickable = true
            setOnClickListener { onClick() }
        }
    }

/** Key / value row: uppercase muted key, right-aligned monospace value. */
fun Context.kv(key: String, value: String, valueColor: Int = color(R.color.text), lines: Int = 1): LinearLayout =
    lin(vertical = false, padH = 14, padV = 0).apply {
        minimumHeight = dp(30)
        setPadding(dp(14), dp(6), dp(14), dp(6))
        addView(label(key.uppercase(), 10f, color(R.color.text_muted), MEDIUM).apply { letterSpacing = 0.1f }, lp(WRAP, WRAP))
        addView(mono(value, 11.5f, valueColor, lines = lines).apply { gravity = Gravity.END }, lp(0, WRAP, 1f).margins(this@kv, l = 12))
    }

/** Round tinted container with an icon in the middle. */
fun Context.iconBadge(res: Int, fg: Int, bg: Int, size: Int = 32): FrameLayout = FrameLayout(this).apply {
    background = rounded(bg, dpf(size * 0.3f), fg and 0x33FFFFFF, dp(1))
    addView(icon(res, fg, size / 2), FrameLayout.LayoutParams(dp(size / 2), dp(size / 2), Gravity.CENTER))
    layoutParams = LinearLayout.LayoutParams(dp(size), dp(size))
}

/** Thin warning strip with a coloured edge and a trailing action. */
fun Context.banner(text: String, action: String, fg: Int, bg: Int, onAction: () -> Unit): LinearLayout =
    lin(vertical = false, padH = 0, padV = 0).apply {
        background = rounded(bg, dpf(12f), fg and 0x4DFFFFFF, dp(1))
        clipToOutline = true
        outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
        addView(View(this@banner).apply { setBackgroundColor(fg) }, lp(dp(3), MATCH))
        addView(icon(R.drawable.ic_warning, fg, 16), lp(dp(16), dp(16)).margins(this@banner, l = 10, r = 8))
        addView(label(text, 12f, color(R.color.text), lines = 3).apply { setPadding(0, dp(9), 0, dp(9)) }, lp(0, WRAP, 1f))
        addView(mono(action.uppercase(), 11f, fg, bold = true).apply {
            setPadding(dp(12), dp(12), dp(12), dp(12))
            isClickable = true
            background = rowRipple()
            setOnClickListener { onAction() }
        })
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
    }

    fun close() {
        isOpen = false
        behind.frost(false)
        scrim.animate().alpha(0f).setDuration(200).withEndAction { scrim.visibility = GONE }.start()
        animate().translationX(-width.toFloat()).setDuration(200).withEndAction {
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
