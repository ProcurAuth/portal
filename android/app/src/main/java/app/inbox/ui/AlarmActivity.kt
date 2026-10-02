package app.inbox.ui

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
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import app.inbox.R
import app.inbox.alert.Alerter
import app.inbox.data.Store

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

    @Deprecated("Back must not dismiss an alarm silently")
    override fun onBackPressed() {
        // Ignored on purpose: the alarm is stopped with the big button only.
    }

    private fun render() {
        val store = Store.get(this)
        val ringing = store.ringingAlarms()
        if (ringing.isEmpty()) return finish()
        // The latest alarm leads; the others are listed below it.
        val a = ringing.last()
        val others = ringing.dropLast(1).reversed()
        val server = store.server(a.serverId)

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

        // Elapsed time since the alert was emitted, live, above everything else.
        emittedAtMs = a.createdAtMs
        col.addView(mono(getString(R.string.alarm_since_alert).uppercase(), 11f, faint, bold = true).apply {
            gravity = Gravity.CENTER
            letterSpacing = 0.14f
        }, lp(MATCH, WRAP).margins(this, t = 12))
        elapsed = mono(elapsedText(System.currentTimeMillis() - a.createdAtMs), 46f, white).apply {
            gravity = Gravity.CENTER
        }
        col.addView(elapsed, lp(MATCH, WRAP).margins(this, t = 6))

        // Header: group and how many others ring.
        val head = lin(vertical = false).apply { gravity = Gravity.CENTER }
        head.addView(chip(a.groupName, red, 0x29F04438, dot = true))
        if (others.isNotEmpty()) {
            head.addView(chip("+" + others.size, dim, 0x14FFFFFF), lp(WRAP, WRAP).margins(this, l = 6))
        }
        col.addView(head, lp(MATCH, WRAP).margins(this, t = 14))
        col.addView(mono(listOfNotNull(server?.serverName?.uppercase(), getString(R.string.alarm_sent_at, hms(a.createdAtMs)),
            getString(R.string.alarm_received_at, hms(a.receivedAtMs)))
            .joinToString(" · "), 10.5f, faint, lines = 2).apply { gravity = Gravity.CENTER; letterSpacing = 0.02f },
            lp(MATCH, WRAP).margins(this, t = 10))

        // Alert text on a glass card, scrollable when long.
        val card = lin(padH = 16, padV = 14).apply {
            background = LayerDrawable(arrayOf(
                rounded(0x17FFFFFF, dpf(18f), 0x24FFFFFF, dp(1)),
                GradientDrawable(GradientDrawable.Orientation.TOP_BOTTOM, intArrayOf(0x14FFFFFF, 0x00FFFFFF)).apply { cornerRadius = dpf(18f) },
            ))
        }
        card.addView(mono(getString(R.string.action_alarm).uppercase() + " · " + a.groupName.uppercase(), 10.5f, red, bold = true).apply { letterSpacing = 0.08f })
        card.addView(label(a.description ?: a.groupName, 22f, white, BOLD), lp().margins(this, t = 8))
        a.details?.let { card.addView(markdown(it, MdPalette.ALARM, 15f), lp().margins(this, t = 10)) }
        if (others.isNotEmpty()) {
            card.addView(View(this).apply { setBackgroundColor(0x1AFFFFFF) }, lp(MATCH, 1).margins(this, t = 12, b = 8))
            for (o in others.take(3)) {
                card.addView(mono(hms(o.createdAtMs) + "  " + (o.description ?: o.groupName), 11.5f, faint), lp().margins(this, t = 3))
            }
            card.addView(label(getString(R.string.alarm_all_stopped), 11f, faint), lp().margins(this, t = 6))
        }
        val textScroll = ScrollView(this).apply {
            isVerticalScrollBarEnabled = false
            addView(card)
        }
        col.addView(textScroll, lp(MATCH, 0, 1f).margins(this, t = 18))

        col.addView(StopButton(this) {
            Alerter.stopAllAlarms(this)
            finish()
        }, lp(WRAP, WRAP).margins(this, t = 8))
        col.addView(mono(getString(R.string.alarm_hint).uppercase(), 11f, dim, bold = true).apply {
            gravity = Gravity.CENTER
            letterSpacing = 0.14f
        }, lp(MATCH, WRAP).margins(this, t = 2, b = 10))

        root.addView(col, FrameLayout.LayoutParams(MATCH, MATCH))
        setContentView(root)
        applyInsets(col)
    }
}
