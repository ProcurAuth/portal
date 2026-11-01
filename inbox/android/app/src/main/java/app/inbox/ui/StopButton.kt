package app.inbox.ui

import android.animation.ValueAnimator
import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.RadialGradient
import android.graphics.Shader
import android.graphics.RectF
import android.os.SystemClock
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.animation.LinearInterpolator
import android.view.animation.OvershootInterpolator
import app.inbox.R
import kotlin.math.hypot
import kotlin.math.min

/**
 * Samsung-style alarm stop control: a big round button inside a threshold ring.
 * Drag it out of the ring (any direction) and release to stop; released inside,
 * it springs back. A 1 s long press, or the accessibility click, also stops it.
 * A simple tap does nothing, so a phone in a pocket cannot silence an alarm.
 */
class StopButton(context: Context, private val onStop: () -> Unit) : View(context) {
    private val red = 0xFFF04438.toInt()
    private val white = 0xFFFFFFFF.toInt()
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = red }
    private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; color = white }
    private val pulse = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; color = red }
    private val glow = Paint(Paint.ANTI_ALIAS_FLAG)
    private val cross = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE; color = white; strokeCap = Paint.Cap.ROUND
    }
    private val arc = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE; color = white; strokeCap = Paint.Cap.ROUND
    }

    private var dx = 0f
    private var dy = 0f
    private var downX = 0f
    private var downY = 0f
    private var dragging = false
    private var pressStart = 0L
    private var done = false
    private var phase = 0f

    private val pulseAnim = ValueAnimator.ofFloat(0f, 1f).apply {
        duration = 1800
        repeatCount = ValueAnimator.INFINITE
        interpolator = LinearInterpolator()
        addUpdateListener { phase = it.animatedValue as Float; invalidate() }
    }

    init {
        contentDescription = context.getString(R.string.alarm_stop_description)
        isClickable = true
        isFocusable = true
        isHapticFeedbackEnabled = true
    }

    /**
     * Smaller dial (about 38 % of the screen width instead of 45 %) so long
     * messages and large fonts keep room above it.
     */
    var compact = false
        set(value) {
            if (field == value) return
            field = value
            requestLayout()
            invalidate()
        }

    /** Inner button radius. */
    private val inner: Float get() = resources.displayMetrics.widthPixels * (if (compact) 0.19f else 0.225f)
    private val outer: Float get() = inner * (if (compact) 1.55f else 1.75f)

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val wanted = (outer * 2 + inner * 0.3f).toInt()
        val maxW = MeasureSpec.getSize(widthMeasureSpec).takeIf { it > 0 } ?: wanted
        val size = min(wanted, maxW)
        setMeasuredDimension(size, size)
    }

    private val arcBounds = RectF()

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        val maxR = min(outer, min(w / 2f, h / 2f))
        glow.shader = RadialGradient(w / 2f, h / 2f, maxR, intArrayOf(0x66F04438, 0x1FF04438, 0x00F04438),
            floatArrayOf(0f, 0.6f, 1f), Shader.TileMode.CLAMP)
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        pulseAnim.start()
    }

    override fun onDetachedFromWindow() {
        pulseAnim.cancel()
        super.onDetachedFromWindow()
    }

    private val outside: Boolean get() = hypot(dx, dy) > outer - inner * 0.35f

    override fun onDraw(canvas: Canvas) {
        val cx = width / 2f
        val cy = height / 2f
        val r = inner
        val maxR = min(outer, min(cx, cy))

        // Soft red glow under the button.
        canvas.drawCircle(cx, cy, maxR, glow)
        // Thin pulsing rings, staggered.
        if (!dragging) {
            pulse.strokeWidth = resources.displayMetrics.density * 1.5f
            for (k in 0..2) {
                val p = (phase + k / 3f) % 1f
                pulse.alpha = ((1f - p) * 170).toInt()
                canvas.drawCircle(cx, cy, r + (maxR - r) * p, pulse)
            }
        }
        // Threshold ring, with tick marks like an instrument dial.
        ring.strokeWidth = resources.displayMetrics.density * 1f
        ring.alpha = if (dragging && outside) 230 else 60
        val rr = maxR - ring.strokeWidth
        canvas.drawCircle(cx, cy, rr, ring)
        for (i in 0 until 48) {
            val ang = Math.toRadians(i * 7.5)
            val len = if (i % 4 == 0) resources.displayMetrics.density * 7f else resources.displayMetrics.density * 3.5f
            val c0 = Math.cos(ang).toFloat()
            val s0 = Math.sin(ang).toFloat()
            canvas.drawLine(cx + c0 * rr, cy + s0 * rr, cx + c0 * (rr - len), cy + s0 * (rr - len), ring)
        }

        // Long-press progress.
        if (pressStart > 0 && !dragging) {
            val p = ((SystemClock.uptimeMillis() - pressStart) / LONG_PRESS_MS.toFloat()).coerceIn(0f, 1f)
            arc.strokeWidth = resources.displayMetrics.density * 5f
            val pad = r + arc.strokeWidth * 2
            arcBounds.set(cx - pad, cy - pad, cx + pad, cy + pad)
            canvas.drawArc(arcBounds, -90f, 360f * p, false, arc)
            if (p < 1f) postInvalidateOnAnimation()
        }

        // The button itself.
        val bx = cx + dx
        val by = cy + dy
        fill.alpha = if (dragging && outside) 200 else 255
        canvas.drawCircle(bx, by, r, fill)
        cross.strokeWidth = resources.displayMetrics.density * 4f
        val c = r * 0.26f
        canvas.drawLine(bx - c, by - c, bx + c, by + c, cross)
        canvas.drawLine(bx + c, by - c, bx - c, by + c, cross)
    }

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(e: MotionEvent): Boolean {
        if (done) return true
        val cx = width / 2f
        val cy = height / 2f
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                if (hypot(e.x - cx, e.y - cy) > inner * 1.15f) return false
                downX = e.x
                downY = e.y
                dragging = false
                pressStart = SystemClock.uptimeMillis()
                postDelayed(longPress, LONG_PRESS_MS)
                parent?.requestDisallowInterceptTouchEvent(true)
                invalidate()
            }
            MotionEvent.ACTION_MOVE -> {
                val mx = e.x - downX
                val my = e.y - downY
                if (!dragging && hypot(mx, my) > resources.displayMetrics.density * 12) {
                    dragging = true
                    pressStart = 0
                    removeCallbacks(longPress)
                }
                if (dragging) {
                    val wasOutside = outside
                    dx = mx
                    dy = my
                    if (outside != wasOutside) performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                    invalidate()
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                removeCallbacks(longPress)
                pressStart = 0
                // The release point counts: a fast flick may end far from the last MOVE.
                if (dragging && e.actionMasked == MotionEvent.ACTION_UP) {
                    dx = e.x - downX
                    dy = e.y - downY
                }
                if (dragging && outside && e.actionMasked == MotionEvent.ACTION_UP) {
                    finish()
                } else {
                    springBack()
                }
                dragging = false
            }
        }
        return true
    }

    private val longPress = Runnable { finish() }

    override fun performClick(): Boolean {
        // Reached through accessibility services only: plain taps do not stop the alarm.
        super.performClick()
        finish()
        return true
    }

    private fun springBack() {
        val sx = dx
        val sy = dy
        ValueAnimator.ofFloat(1f, 0f).apply {
            duration = 350
            interpolator = OvershootInterpolator(2f)
            addUpdateListener {
                val f = it.animatedValue as Float
                dx = sx * f
                dy = sy * f
                invalidate()
            }
            start()
        }
    }

    private fun finish() {
        if (done) return
        done = true
        performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
        animate().alpha(0f).scaleX(0.8f).scaleY(0.8f).setDuration(180).withEndAction { onStop() }.start()
    }

    companion object {
        private const val LONG_PRESS_MS = 1000L
    }
}
