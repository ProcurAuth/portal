package app.inbox.ui

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.WindowManager
import android.widget.LinearLayout
import app.inbox.alert.Alerter
import app.inbox.data.Store

/** Full-screen alarm shown over the lock screen until a human acknowledges it. */
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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 27) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        } else {
            @Suppress("DEPRECATION")
            window.addFlags(WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON)
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        render(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        render(intent)
    }

    private fun render(intent: Intent) {
        val serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val alertId = intent.getStringExtra(EXTRA_ALERT)
        val store = Store.get(this)
        val a = alertId?.let { store.alert(serverId, it) } ?: return showNext()
        if (a.ackedAtMs != null) return showNext()
        val server = store.server(serverId)
        val root = column(24).apply {
            gravity = Gravity.CENTER
            setBackgroundColor(RED)
        }
        val white = 0xFFFFFFFF.toInt()
        root.addView(text("ALARME", 34f, bold = true, color = white).apply { gravity = Gravity.CENTER })
        root.addView(text(a.groupName, 20f, color = white).apply { gravity = Gravity.CENTER })
        root.addView(text(a.title ?: a.groupName, 26f, bold = true, color = white).apply { gravity = Gravity.CENTER })
        a.message?.let { root.addView(text(it, 18f, color = white).apply { gravity = Gravity.CENTER }) }
        root.addView(text("${server?.serverName ?: ""} · ${formatTime(a.createdAtMs)}", 14f, color = white).apply { gravity = Gravity.CENTER })
        val ack = button("ACQUITTER") {
            Alerter.acknowledge(this, serverId, a.alertId)
            showNext()
        }.apply {
            textSize = 24f
            minHeight = dp(96)
        }
        root.addView(ack, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply {
            topMargin = dp(32)
        })
        setContentView(root)
    }

    /** Shows the next unacknowledged alarm, or closes. */
    private fun showNext() {
        val next = Store.get(this).unackedAlarms().lastOrNull()
        if (next != null) {
            render(intent(this, next.serverId, next.alertId))
        } else {
            finish()
        }
    }
}
