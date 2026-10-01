package app.inbox.ui

import android.app.AlertDialog
import android.os.Bundle
import android.view.inputmethod.EditorInfo
import android.widget.LinearLayout
import android.widget.Toast
import app.inbox.R
import app.inbox.data.Store
import app.inbox.service.LinkState
import app.inbox.service.InboxService
import app.inbox.service.Registry

/** One server: status, pseudo, and what each group does on this phone. */
class ServerActivity : BaseActivity() {
    companion object {
        const val EXTRA_SERVER = "server_id"
        private val ACTIONS = listOf<String?>(null, "notify", "ring", "alarm")
    }

    private var serverId = -1L
    private lateinit var body: LinearLayout
    private val onChange: () -> Unit = { render() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val root = lin()
        val server = Store.get(this).server(serverId) ?: return finish()
        root.addView(topBar(server.serverName, R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }))
        body = lin(padH = 16, padV = 8)
        root.addView(scroll(body), lp(MATCH, 0, 1f))
        setScreen(root)
    }

    override fun onResume() {
        super.onResume()
        Registry.addListener(onChange)
        render()
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
    }

    private fun render() {
        val store = Store.get(this)
        val server = store.server(serverId) ?: return finish()
        body.removeAllViews()

        // Status
        val st = Registry.status(serverId)
        val (text, fg, bg) = when (st.state) {
            LinkState.CONNECTED -> Triple(getString(R.string.status_connected), color(R.color.success), color(R.color.success_soft))
            LinkState.CONNECTING -> Triple(getString(R.string.status_connecting), color(R.color.warning), color(R.color.warning_soft))
            LinkState.DISCONNECTED -> Triple(
                getString(R.string.status_disconnected_since, formatSince(System.currentTimeMillis() - st.sinceMs)),
                color(R.color.danger), color(R.color.danger_soft),
            )
            LinkState.REENROLL -> Triple(getString(R.string.status_reenroll), color(R.color.danger), color(R.color.danger_soft))
        }
        val head = card(16).apply { orientation = LinearLayout.HORIZONTAL }
        head.addView(iconBadge(R.drawable.ic_server, color(R.color.primary), color(R.color.primary_soft), 44), lp(dp(44), dp(44)).margins(this, r = 14))
        val col = lin()
        col.addView(label(server.serverName, 17f, color(R.color.text_strong), MEDIUM))
        col.addView(label(server.address, 13f, color(R.color.text_muted)), lp().margins(this, t = 2))
        col.addView(label(getString(R.string.server_device, server.deviceName), 13f, color(R.color.text_muted)), lp().margins(this, t = 2))
        col.addView(pill(text, fg, bg, dot = true), lp(WRAP, WRAP).margins(this, t = 10))
        if (st.state != LinkState.CONNECTED && st.detail != null) {
            col.addView(label(st.detail, 12f, color(R.color.text_muted)), lp().margins(this, t = 6))
        }
        head.addView(col, lp(0, WRAP, 1f))
        body.addView(head, lp().margins(this, t = 8))

        // Pseudo on this server
        body.addView(sectionLabel(getString(R.string.server_pseudo)))
        val p = card(16)
        p.addView(label(getString(R.string.server_pseudo_help), 13f, color(R.color.text_muted)))
        val input = textField(server.userName ?: "", getString(R.string.pseudo_hint))
        p.addView(input, lp().margins(this, t = 12))
        val save = {
            val v = cleanUserName(input.text.toString())
            if (v == null) {
                input.error = getString(R.string.pseudo_invalid)
            } else {
                store.setUserName(serverId, v)
                Registry.connections[serverId]?.setUserName(v)
                Toast.makeText(this, getString(R.string.saved), Toast.LENGTH_SHORT).show()
            }
        }
        input.setOnEditorActionListener { _, action, _ -> if (action == EditorInfo.IME_ACTION_DONE) { save(); true } else false }
        p.addView(button(getString(R.string.save), ButtonKind.SECONDARY) { save() }, lp().margins(this, t = 12))
        body.addView(p)

        // Groups
        body.addView(sectionLabel(getString(R.string.server_groups)))
        body.addView(label(getString(R.string.server_groups_hint), 13f, color(R.color.text_muted)).apply { setPadding(dp(4), 0, dp(4), dp(10)) })
        val subs = store.subscriptions(serverId)
        val groups = store.groups(serverId)
        if (groups.isEmpty()) {
            body.addView(label(getString(R.string.server_groups_none), 14f, color(R.color.text_muted)).apply { setPadding(dp(4), 0, 0, dp(8)) })
        }
        val labels = listOf(
            getString(R.string.action_off_short), getString(R.string.action_notify_short),
            getString(R.string.action_ring), getString(R.string.action_alarm),
        )
        val helps = listOf(R.string.action_off_help, R.string.action_notify_help, R.string.action_ring_help, R.string.action_alarm_help)
        for (g in groups) {
            val current = ACTIONS.indexOf(subs[g.id]).coerceAtLeast(0)
            val c = card(16)
            val top = lin(vertical = false)
            val style = actionStyle(ACTIONS[current])
            top.addView(iconBadge(style.icon, style.fg, style.bg, 36), lp(dp(36), dp(36)).margins(this, r = 12))
            val t = lin()
            t.addView(label(g.name, 15f, color(R.color.text_strong), MEDIUM))
            if (g.description.isNotEmpty()) t.addView(label(g.description, 13f, color(R.color.text_muted)), lp().margins(this, t = 2))
            top.addView(t, lp(0, WRAP, 1f))
            c.addView(top)
            c.addView(segmented(labels, current, accent = { i ->
                val s = actionStyle(ACTIONS[i])
                s.fg to s.bg
            }) { i ->
                if (Registry.connections[serverId]?.subscribe(g.id, ACTIONS[i]) != true) {
                    Toast.makeText(this, getString(R.string.server_offline_change), Toast.LENGTH_LONG).show()
                }
            }, lp().margins(this, t = 12))
            c.addView(label(getString(helps[current]), 12f, color(R.color.text_muted)), lp().margins(this, t = 8))
            body.addView(c, lp().margins(this, b = 10))
        }

        body.addView(button(getString(R.string.server_remove), ButtonKind.DANGER) { confirmRemove(server.serverName) },
            lp().margins(this, t = 16, b = 24))
    }

    private fun confirmRemove(name: String) {
        AlertDialog.Builder(this)
            .setTitle(getString(R.string.server_remove_title, name))
            .setMessage(getString(R.string.server_remove_body))
            .setPositiveButton(getString(R.string.server_remove_confirm)) { _, _ ->
                Registry.connections.remove(serverId)?.stop()
                Store.get(this).deleteServer(serverId)
                Registry.forget(serverId)
                InboxService.start(this)
                finish()
            }
            .setNegativeButton(getString(R.string.cancel), null)
            .show()
    }
}
