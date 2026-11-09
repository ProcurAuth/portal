package app.portalinbox.ui

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.LayerDrawable
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.SpannableStringBuilder
import android.text.StaticLayout
import android.text.Spanned
import android.text.style.TypefaceSpan
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.ScrollView
import android.widget.TextView
import android.window.OnBackInvokedDispatcher
import app.portalinbox.R
import app.portalinbox.alert.Alerter
import app.portalinbox.data.Store

/**
 * Full-screen alarm, over the lock screen, until it is stopped on the phone.
 * Shows the latest ringing alarm with its full message and how many others ring;
 * stopping silences all of them. Purely local: alerts are one-way.
 */
class AlarmActivity : Activity() {
    companion object {
        private const val EXTRA_SERVER = "server_id"
        private const val EXTRA_ALERT = "alert_id"

        fun intent(context: Context, serverId: Long, alertId: String): Intent =
            Intent(context, AlarmActivity::class.java)
                .putExtra(EXTRA_SERVER, serverId)
                .putExtra(EXTRA_ALERT, alertId)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_USER_ACTION)
    }

    private val ticker = Handler(Looper.getMainLooper())
    private var elapsed: TextView? = null
    private var emittedAtMs = 0L
    private val tick = object : Runnable {
        override fun run() {
            elapsed?.text = elapsedText(System.currentTimeMillis() - emittedAtMs)
            ticker.postDelayed(this, 1000 - System.currentTimeMillis() % 1000)
        }
    }

    private val white = 0xFFFFFFFF.toInt()
    private val dim = 0xB3FFFFFF.toInt()
    private val faint = 0x80FFFFFF.toInt()

    @Suppress("DEPRECATION")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 27) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        } else {
            window.addFlags(WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON)
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.decorView.systemUiVisibility = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
            View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
        blockBack()
        render()
    }

    override fun onResume() {
        super.onResume()
        ticker.removeCallbacks(tick)
        ticker.post(tick)
        // The page is visible: the notification must not cover it with a heads-up.
        Alerter.quietAlarmNotifications(this, Store.get(this).ringingAlarms())
    }

    override fun onPause() {
        super.onPause()
        ticker.removeCallbacks(tick)
    }

    /** Time since the alert was emitted: M:SS under an hour, then H:MM:SS. */
    private fun elapsedText(ms: Long): String {
        val total = (ms.coerceAtLeast(0) / 1000)
        val h = total / 3600
        val m = (total % 3600) / 60
        val s = total % 60
        return if (h > 0) "+%d:%02d:%02d".format(h, m, s) else "+%02d:%02d".format(m, s)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        render()
    }

    /** Android 8 to 12: Back is ignored on purpose (13+ uses the callback registered in onCreate). */
    @Deprecated("Back must not dismiss an alarm silently")
    override fun onBackPressed() {
        // Ignored on purpose: the alarm is stopped with the big button only.
    }

    /** Swallows Back on Android 13+: the alarm is stopped with the big button only. */
    private fun blockBack() {
        if (Build.VERSION.SDK_INT >= 33) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_OVERLAY) {}
        }
    }

    private fun render() {
        val store = Store.get(this)
        val ringing = store.ringingAlarms()
        if (ringing.isEmpty()) return finish()
        // The latest alarm leads; the others are listed below it.
        val a = ringing.last()
        val others = ringing.dropLast(1).reversed()
        val server = store.server(a.serverId)
        val largeFont = resources.configuration.fontScale >= 1.2f

        val red = 0xFFF04438.toInt()
        val glow = GradientDrawable().apply {
            gradientType = GradientDrawable.RADIAL_GRADIENT
            colors = intArrayOf(0x66F04438, 0x26F04438, 0x00F04438)
            gradientRadius = resources.displayMetrics.widthPixels * 0.95f
            setGradientCenter(0.5f, 0.76f)
        }
        val root = FrameLayout(this).apply {
            background = LayerDrawable(arrayOf(ColorDrawable(0xFF0B0E13.toInt()), glow))
        }
        val col = lin(padH = 16, padV = 16).apply { gravity = Gravity.CENTER_HORIZONTAL }

        // Elapsed time since the alert was emitted, live: "+03:12 since alert".
        emittedAtMs = a.createdAtMs
        val timer = lin(vertical = false).apply {
            gravity = Gravity.CENTER
            isBaselineAligned = true
        }
        elapsed = mono(elapsedText(System.currentTimeMillis() - a.createdAtMs), if (largeFont) 40f else 46f, white)
        timer.addView(elapsed)
        timer.addView(label(getString(R.string.alarm_since_alert), SP_L, dim), lp(WRAP, WRAP).margins(this, l = 10))
        col.addView(timer, lp(MATCH, WRAP).margins(this, t = 8))

        // Group (the one place it is named) and how many others ring.
        val head = lin(vertical = false).apply { gravity = Gravity.CENTER }
        head.addView(label(a.groupName, SP_M, red, MEDIUM, lines = 1).apply {
            background = rounded(0x29F04438, dpf(CHIP_RADIUS), 0x40F04438, dp(1))
            setPadding(dp(8), dp(4), dp(10), dp(4))
            val d = rounded(red, dpf(3f)).apply { setSize(dp(6), dp(6)) }
            setCompoundDrawablesRelativeWithIntrinsicBounds(d, null, null, null)
            compoundDrawablePadding = dp(6)
        })
        if (others.isNotEmpty()) {
            head.addView(mono("+" + others.size, SP_M, dim, bold = true).apply {
                background = rounded(0x14FFFFFF, dpf(CHIP_RADIUS))
                setPadding(dp(8), dp(4), dp(8), dp(4))
                contentDescription = resources.getQuantityString(R.plurals.alarm_others, others.size, others.size)
            }, lp(WRAP, WRAP).margins(this, l = 6))
        }
        col.addView(head, lp(MATCH, WRAP).margins(this, t = 12))
        val meta = SpannableStringBuilder()
        server?.nickname?.let { meta.append(it).append("  ·  ") }
        meta.append(getString(R.string.alarm_sent)).append(' ')
        monoSpan(meta, hms(a.createdAtMs))
        meta.append("  ·  ").append(getString(R.string.alarm_received)).append(' ')
        monoSpan(meta, hms(a.receivedAtMs))
        col.addView(label(meta, SP_S, faint, lines = 2).apply { gravity = Gravity.CENTER },
            lp(MATCH, WRAP).margins(this, t = 10))

        // Alert text on a card; scrolls when long, with a fade and a "More" hint.
        val card = lin(padH = 16, padV = 14).apply {
            background = LayerDrawable(arrayOf(
                rounded(0x17FFFFFF, dpf(18f), 0x24FFFFFF, dp(1)),
                GradientDrawable(GradientDrawable.Orientation.TOP_BOTTOM, intArrayOf(0x14FFFFFF, 0x00FFFFFF)).apply { cornerRadius = dpf(18f) },
            ))
        }
        val title = label(a.title, 24f, white, BOLD)
        // Beyond two lines at 24 sp, the title steps down to 18 sp.
        val textWidth = resources.displayMetrics.widthPixels - dp(64)
        val lines = StaticLayout.Builder.obtain(a.title, 0, a.title.length, title.paint, textWidth)
            .setLineSpacing(title.lineSpacingExtra, title.lineSpacingMultiplier)
            .build().lineCount
        if (lines > 2) title.textSize = 18f
        card.addView(title)
        if (others.isNotEmpty()) {
            card.addView(View(this).apply { setBackgroundColor(0x1AFFFFFF) }, lp(MATCH, 1).margins(this, t = 12, b = 8))
            for (o in others.take(3)) {
                val line = SpannableStringBuilder()
                monoSpan(line, hms(o.createdAtMs))
                line.append("  ").append(o.title)
                card.addView(label(line, SP_S, faint, lines = 1), lp().margins(this, t = 4))
            }
            card.addView(label(getString(R.string.alarm_all_stopped), SP_S, faint), lp().margins(this, t = 6))
        }
        val textScroll = ScrollView(this).apply {
            isVerticalScrollBarEnabled = false
            isVerticalFadingEdgeEnabled = true
            setFadingEdgeLength(dp(40))
            addView(card)
        }
        val more = label(getString(R.string.alarm_more), SP_S, white, MEDIUM).apply {
            background = rounded(0xCC1A1F29.toInt(), dpf(12f), 0x33FFFFFF, dp(1))
            setPadding(dp(12), dp(5), dp(12), dp(5))
            visibility = View.GONE
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
            isClickable = true
            setOnClickListener { textScroll.smoothScrollBy(0, textScroll.height * 2 / 3) }
        }
        val textBox = FrameLayout(this)
        textBox.addView(textScroll, FrameLayout.LayoutParams(MATCH, MATCH))
        textBox.addView(more, FrameLayout.LayoutParams(WRAP, WRAP, Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL).apply {
            bottomMargin = dp(6)
        })
        col.addView(textBox, lp(MATCH, 0, 1f).margins(this, t = 16))

        val stop = StopButton(this) {
            Alerter.stopAllAlarms(this)
            finish()
        }.apply { compact = largeFont }
        // Overflowing text: show the hint and give the text more room with a smaller dial.
        val refreshMore = {
            val overflow = card.height > textScroll.height
            if (overflow) stop.compact = true
            more.visibility = if (textScroll.canScrollVertically(1)) View.VISIBLE else View.GONE
        }
        textScroll.setOnScrollChangeListener { _, _, _, _, _ -> refreshMore() }
        textScroll.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> textScroll.post { refreshMore() } }
        col.addView(stop, lp(WRAP, WRAP).margins(this, t = 8))
        col.addView(label(getString(R.string.alarm_hint), SP_M, dim, MEDIUM).apply {
            gravity = Gravity.CENTER
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }, lp(MATCH, WRAP).margins(this, t = 2, b = 10))

        root.addView(col, FrameLayout.LayoutParams(MATCH, MATCH))
        setContentView(root)
        applyInsets(col)
    }

    private fun monoSpan(sb: SpannableStringBuilder, text: String) {
        val start = sb.length
        sb.append(text)
        sb.setSpan(TypefaceSpan("monospace"), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
    }
}
