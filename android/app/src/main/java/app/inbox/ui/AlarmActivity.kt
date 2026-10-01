package app.inbox.ui

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextClock
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

        val root = FrameLayout(this).apply {
            background = GradientDrawable(
                GradientDrawable.Orientation.TOP_BOTTOM,
                intArrayOf(0xFF0F141B.toInt(), 0xFF1A1218.toInt(), 0xFF3B0D10.toInt()),
            )
        }
        val col = lin(padH = 28, padV = 24).apply { gravity = Gravity.CENTER_HORIZONTAL }

        // Header: group and how many others ring.
        val head = lin(vertical = false).apply { gravity = Gravity.CENTER }
        head.addView(pill(a.groupName, white, 0x33E5484D, dot = true))
        if (others.isNotEmpty()) {
            head.addView(
                pill(getString(R.string.alarm_more, others.size), dim, 0x1FFFFFFF),
                lp(WRAP, WRAP).margins(this, l = 8),
            )
        }
        col.addView(head, lp(MATCH, WRAP).margins(this, t = 24))

        col.addView(TextClock(this).apply {
            format12Hour = "HH:mm"
            format24Hour = "HH:mm"
            textSize = 64f
            typeface = LIGHT
            setTextColor(white)
            gravity = Gravity.CENTER
            includeFontPadding = false
        }, lp(MATCH, WRAP).margins(this, t = 16))
        col.addView(label(relativeTime(a.createdAtMs).replaceFirstChar { it.uppercase() } +
            " · " + (server?.serverName ?: ""), 14f, faint).apply { gravity = Gravity.CENTER },
            lp(MATCH, WRAP).margins(this, t = 6))

        // Alert text, scrollable when long.
        val text = lin().apply { gravity = Gravity.CENTER_HORIZONTAL }
        text.addView(label(a.title ?: a.groupName, 28f, white, BOLD).apply { gravity = Gravity.CENTER })
        a.message?.let {
            text.addView(label(it, 17f, dim).apply { gravity = Gravity.CENTER }, lp(MATCH, WRAP).margins(this, t = 12))
        }
        for (o in others.take(3)) {
            text.addView(label("• " + (o.title ?: o.groupName), 14f, faint, lines = 1).apply {
                gravity = Gravity.CENTER
            }, lp(MATCH, WRAP).margins(this, t = 8))
        }
        if (others.isNotEmpty()) {
            text.addView(label(getString(R.string.alarm_all_stopped), 12f, faint).apply { gravity = Gravity.CENTER },
                lp(MATCH, WRAP).margins(this, t = 8))
        }
        val textScroll = ScrollView(this).apply {
            isVerticalScrollBarEnabled = false
            addView(text)
        }
        col.addView(textScroll, lp(MATCH, 0, 1f).margins(this, t = 20))

        col.addView(StopButton(this) {
            Alerter.stopAllAlarms(this)
            finish()
        }, lp(WRAP, WRAP).margins(this, t = 12))
        col.addView(label(getString(R.string.alarm_hint), 15f, dim, MEDIUM).apply { gravity = Gravity.CENTER },
            lp(MATCH, WRAP).margins(this, t = 4, b = 12))

        root.addView(col, FrameLayout.LayoutParams(MATCH, MATCH))
        setContentView(root)
        applyInsets(col)
    }
}
