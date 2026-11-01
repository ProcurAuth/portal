package app.inbox.ui

import android.animation.Animator
import android.animation.AnimatorListenerAdapter
import android.animation.ValueAnimator
import android.content.Context
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.VelocityTracker
import android.view.View
import android.view.ViewConfiguration
import android.view.accessibility.AccessibilityNodeInfo
import android.view.animation.DecelerateInterpolator
import android.widget.FrameLayout
import app.inbox.R
import kotlin.math.abs

/**
 * Wraps an alert card so that dragging it to the right marks it as read.
 *
 * Behind the card sits an accent background with a check and "Read". Releasing
 * past [THRESHOLD] of the width, or flinging right, commits; anything else springs
 * back. Only clearly horizontal drags are intercepted, so vertical scrolling, taps
 * and long presses on the card keep working.
 *
 * @param enabled false for an alert that is already read: the card does not move.
 * @param removeOnCommit true when read alerts are hidden: the card slides out and
 *   the row collapses before [onCommit] runs; otherwise it springs back first.
 */
// Built in code only, never inflated from XML.
@android.annotation.SuppressLint("ViewConstructor")
class SwipeToRead(
    context: Context,
    private val card: View,
    private val enabled: Boolean,
    private val removeOnCommit: Boolean,
    private val onCommit: () -> Unit,
) : FrameLayout(context) {

    private val slop = ViewConfiguration.get(context).scaledTouchSlop
    private val minFling = ViewConfiguration.get(context).scaledMinimumFlingVelocity * 6
    private var downX = 0f
    private var downY = 0f
    private var dragging = false
    /** Past the commit threshold: releasing now marks read (a haptic tick says so). */
    private var armed = false
    private var done = false
    private var velocity: VelocityTracker? = null

    init {
        val radius = context.dpf(CARD_RADIUS)
        val reveal = context.lin(vertical = false).apply {
            gravity = Gravity.CENTER_VERTICAL
            setPadding(context.dp(16), 0, context.dp(16), 0)
            background = rounded(context.color(R.color.primary), radius)
            addView(context.icon(R.drawable.ic_check, context.color(R.color.on_primary), 18), lp(context.dp(18), context.dp(18)))
            addView(context.label(context.getString(R.string.swipe_read), SP_M, context.color(R.color.on_primary), MEDIUM),
                lp(WRAP, WRAP).margins(context, l = 8))
            visibility = INVISIBLE
        }
        addView(reveal, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(card, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT))
        if (enabled) {
            accessibilityDelegate = object : AccessibilityDelegate() {
                override fun onInitializeAccessibilityNodeInfo(host: View, info: AccessibilityNodeInfo) {
                    super.onInitializeAccessibilityNodeInfo(host, info)
                    info.addAction(AccessibilityNodeInfo.AccessibilityAction(
                        R.id.action_mark_read, context.getString(R.string.alert_mark_read)))
                }

                override fun performAccessibilityAction(host: View, action: Int, args: Bundle?): Boolean {
                    if (action == R.id.action_mark_read) {
                        commit()
                        return true
                    }
                    return super.performAccessibilityAction(host, action, args)
                }
            }
        }
    }

    private val reveal: View get() = getChildAt(0)

    override fun onInterceptTouchEvent(ev: MotionEvent): Boolean {
        if (!enabled || done) return false
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downX = ev.x
                downY = ev.y
                dragging = false
                velocity?.recycle()
                velocity = VelocityTracker.obtain().also { it.addMovement(ev) }
            }
            MotionEvent.ACTION_MOVE -> {
                velocity?.addMovement(ev)
                val dx = ev.x - downX
                val dy = ev.y - downY
                if (dx > slop && dx > abs(dy)) {
                    dragging = true
                    parent?.requestDisallowInterceptTouchEvent(true)
                    reveal.visibility = VISIBLE
                    return true
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> recycle()
        }
        return false
    }

    // The drag is a gesture, not a click; accessibility uses the "Mark as read" action.
    @android.annotation.SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(ev: MotionEvent): Boolean {
        if (!enabled || done) return false
        velocity?.addMovement(ev)
        when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN -> return true
            MotionEvent.ACTION_MOVE -> {
                if (!dragging) return true
                val dx = (ev.x - downX).coerceAtLeast(0f)
                card.translationX = dx
                val past = dx > width * THRESHOLD
                if (past != armed) {
                    armed = past
                    if (past) performHapticFeedback(
                        if (Build.VERSION.SDK_INT >= 30) HapticFeedbackConstants.CONFIRM else HapticFeedbackConstants.CLOCK_TICK)
                }
            }
            MotionEvent.ACTION_UP -> {
                if (dragging) {
                    val dx = ev.x - downX
                    velocity?.computeCurrentVelocity(1000)
                    val vx = velocity?.xVelocity ?: 0f
                    if (dx > width * THRESHOLD || (vx > minFling && dx > slop * 2)) commit() else springBack(null)
                }
                recycle()
            }
            MotionEvent.ACTION_CANCEL -> {
                if (dragging) springBack(null)
                recycle()
            }
        }
        return true
    }

    private fun recycle() {
        dragging = false
        armed = false
        velocity?.recycle()
        velocity = null
    }

    private fun commit() {
        if (done) return
        done = true
        reveal.visibility = VISIBLE
        if (removeOnCommit) {
            card.animate().translationX(width.toFloat()).setDuration(160)
                .setInterpolator(DecelerateInterpolator())
                .withEndAction { collapse() }
                .start()
        } else {
            springBack { onCommit() }
        }
    }

    private fun springBack(then: (() -> Unit)?) {
        card.animate().translationX(0f).setDuration(180)
            .setInterpolator(DecelerateInterpolator())
            .withEndAction {
                reveal.visibility = INVISIBLE
                then?.invoke()
            }
            .start()
    }

    /** Shrinks the row (and its top margin) to nothing, then commits. */
    private fun collapse() {
        val params = layoutParams as? android.view.ViewGroup.MarginLayoutParams
        val startHeight = height
        val startMargin = params?.topMargin ?: 0
        ValueAnimator.ofFloat(1f, 0f).apply {
            duration = 180
            interpolator = DecelerateInterpolator()
            addUpdateListener {
                val f = it.animatedValue as Float
                params?.let { p ->
                    p.height = (startHeight * f).toInt()
                    p.topMargin = (startMargin * f).toInt()
                    layoutParams = p
                }
                alpha = f
            }
            addListener(object : AnimatorListenerAdapter() {
                override fun onAnimationEnd(animation: Animator) = onCommit()
            })
        }.start()
    }

    companion object {
        /** Share of the width past which a release commits. */
        const val THRESHOLD = 0.35f
    }
}
