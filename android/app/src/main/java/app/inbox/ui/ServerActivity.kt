package app.inbox.ui

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
    private lateinit var bar: android.widget.FrameLayout
    private lateinit var body: LinearLayout
    private val onChange: () -> Unit = { render() }
    private val handler = android.os.Handler(android.os.Looper.getMainLooper())
    /** Live rows, refreshed every second without rebuilding the screen. */
    private var pingValue: android.widget.TextView? = null
    private var updateValue: android.widget.TextView? = null
    private var sinceValue: android.widget.TextView? = null
    private val tick = object : Runnable {
        override fun run() {
            refreshLive()
            handler.postDelayed(this, 1_000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val root = lin()
        val server = Store.get(this).server(serverId) ?: return finish()
        title = server.serverName
        bar = android.widget.FrameLayout(this)
        root.addView(bar, lp())
        body = lin(padH = 10, padV = 4)
        root.addView(scroll(body), lp(MATCH, 0, 1f))
        setScreen(root)
    }

    override fun onResume() {
        super.onResume()
        Registry.addListener(onChange)
        render()
        handler.post(tick)
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
        handler.removeCallbacks(tick)
    }

    private fun refreshLive() {
        val now = System.currentTimeMillis()
        val live = Registry.liveness(serverId)
        val st = Registry.status(serverId)
        val connected = st.state == LinkState.CONNECTED
        pingValue?.text = live.lastPongMs?.let { at ->
            getString(R.string.server_ago, liveAge(at, now)) + (live.rttMs?.let { "  ·  $it ms" } ?: "")
        } ?: getString(R.string.server_never)
        pingValue?.setTextColor(color(if (connected) R.color.text else R.color.text_muted))
        updateValue?.text = live.lastFrameMs?.let { "${hms(it)}  ·  " + getString(R.string.server_ago, liveAge(it, now)) }
            ?: getString(R.string.server_never)
        sinceValue?.text = if (connected) "${hms(st.sinceMs)}  ·  ${liveAge(st.sinceMs, now)}" else "—"
    }

    private fun render() {
        val store = Store.get(this)
        val server = store.server(serverId) ?: return finish()
        body.removeAllViews()

        // Status
        val st = Registry.status(serverId)
        val now = System.currentTimeMillis()
        val (code, fg) = when (st.state) {
            LinkState.CONNECTED -> getString(R.string.status_online) to color(R.color.primary)
            LinkState.CONNECTING -> getString(R.string.status_connecting) to color(R.color.warning)
            LinkState.DISCONNECTED -> getString(R.string.status_offline_since, shortAge(st.sinceMs, now)) to color(R.color.danger)
            LinkState.REENROLL -> getString(R.string.status_reenroll) to color(R.color.danger)
        }
        bar.removeAllViews()
        val short = when (st.state) {
            LinkState.DISCONNECTED -> getString(R.string.status_offline)
            else -> code
        }
        // Same status light as the home: words stay in the State row below.
        val light = PulseDot(this, if (st.state == LinkState.CONNECTED) color(R.color.primary) else fg, st.state == LinkState.CONNECTED)
            .apply { contentDescription = short }
        bar.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }, light))
        // Live: heartbeat round trip and last frame, ticking every second.
        body.addView(sectionLabel(getString(R.string.server_live)))
        val live = panel(padV = 4)
        live.addView(kv(getString(R.string.server_last_ping), "—"))
        pingValue = (live.getChildAt(live.childCount - 1) as LinearLayout).let { it.getChildAt(it.childCount - 1) as android.widget.TextView }
        live.addView(divider())
        live.addView(kv(getString(R.string.server_last_update), "—"))
        updateValue = (live.getChildAt(live.childCount - 1) as LinearLayout).let { it.getChildAt(it.childCount - 1) as android.widget.TextView }
        live.addView(divider())
        live.addView(kv(getString(R.string.server_connected_since), "—"))
        sinceValue = (live.getChildAt(live.childCount - 1) as LinearLayout).let { it.getChildAt(it.childCount - 1) as android.widget.TextView }
        body.addView(live)
        refreshLive()
        // Screen context: which server, where.
        body.addView(sectionLabel(server.serverName, server.address.removePrefix("http://").removePrefix("https://")))
        val head = panel(padV = 4)
        head.addView(kv(getString(R.string.meta_state), code, fg))
        head.addView(divider())
        head.addView(kv(getString(R.string.meta_server), server.serverName))
        head.addView(divider())
        head.addView(kv(getString(R.string.meta_address), server.address))
        head.addView(divider())
        head.addView(kv(getString(R.string.meta_device), server.deviceName))
        head.addView(divider())
        head.addView(kv(getString(R.string.meta_pseudo), server.userName ?: "—"))
        head.addView(divider())
        head.addView(kv(getString(R.string.meta_device_id), server.deviceId.take(16) + "…"))
        if (st.state != LinkState.CONNECTED && st.detail != null) {
            head.addView(divider())
            head.addView(kv(getString(R.string.meta_detail), st.detail, color(R.color.text_muted), lines = 3))
        }
        body.addView(head)

        // Pseudo on this server
        body.addView(sectionLabel(getString(R.string.server_pseudo)))
        val p = panel(padH = 14, padV = 12)
        p.addView(label(getString(R.string.server_pseudo_help), 11.5f, color(R.color.text_muted)))
        p.addView(pseudoEditor(server.userName) { v ->
            store.setUserName(serverId, v)
            Registry.connections[serverId]?.setUserName(v)
            Toast.makeText(this, getString(R.string.saved), Toast.LENGTH_SHORT).show()
            render()
        }, lp().margins(this, t = 10))
        body.addView(p)

        // Groups
        val subs = store.subscriptions(serverId) // local levels; the server only knows the group ids
        val groups = store.groups(serverId)
        body.addView(sectionLabel(getString(R.string.server_groups), if (groups.isEmpty()) null else
            getString(R.string.server_groups_subscribed, groups.count { subs[it.id] != null }, groups.size)))
        body.addView(label(getString(R.string.server_groups_hint), 11.5f, color(R.color.text_muted)).apply { setPadding(dp(6), 0, dp(6), dp(8)) })
        if (groups.isEmpty()) {
            body.addView(label(getString(R.string.server_groups_none), 12.5f, color(R.color.text_muted)).apply { setPadding(dp(6), 0, 0, dp(8)) })
        } else {
            val labels = listOf(
                getString(R.string.seg_off), getString(R.string.seg_notify), getString(R.string.seg_ring), getString(R.string.seg_alarm),
            )
            val helps = listOf(R.string.action_off_help, R.string.action_notify_help, R.string.action_ring_help, R.string.action_alarm_help)
            val list = panel()
            groups.forEachIndexed { i, g ->
                if (i > 0) list.addView(divider())
                val current = ACTIONS.indexOf(subs[g.id]).coerceAtLeast(0)
                val style = actionStyle(ACTIONS[current])
                val c = lin(vertical = false)
                c.addView(android.view.View(this).apply { setBackgroundColor(if (current == 0) 0x00000000 else style.fg) }, lp(dp(3), MATCH))
                val col = lin(padH = 12, padV = 10)
                val top = lin(vertical = false)
                val t = lin()
                t.addView(label(g.name, 14f, color(R.color.text_strong), MEDIUM, lines = 1))
                val desc = if (g.description.isNotEmpty()) "${g.id} · ${g.description}" else g.id
                t.addView(mono(desc, 10.5f, color(R.color.text_subtle)), lp().margins(this, t = 2))
                top.addView(t, lp(0, WRAP, 1f))
                top.addView(chip(style.code, style.fg, style.bg))
                col.addView(top)
                col.addView(segmented(labels, current, accent = { k ->
                    val s = actionStyle(ACTIONS[k])
                    s.fg to s.bg
                }) { k ->
                    // Saved on the phone right away; the server learns the subscription when connected.
                    val conn = Registry.connections[serverId]
                    if (conn != null) conn.setLevel(g.id, ACTIONS[k]) else {
                        store.setLevel(serverId, g.id, ACTIONS[k])
                        render()
                    }
                }, lp().margins(this, t = 8))
                col.addView(label(getString(helps[current]), 11f, color(R.color.text_muted)), lp().margins(this, t = 6))
                c.addView(col, lp(0, WRAP, 1f))
                list.addView(c)
            }
            body.addView(list)
        }

        body.addView(button(getString(R.string.server_remove), ButtonKind.DANGER) { confirmRemove(server.serverName) },
            lp().margins(this, t = 16, b = 20))
    }

    private fun confirmRemove(name: String) {
        bottomSheet { sheet, dialog ->
            sheet.addView(chip(getString(R.string.server_remove_confirm), color(R.color.danger), color(R.color.danger_soft), dot = true),
                lp(WRAP, WRAP))
            sheet.addView(label(getString(R.string.server_remove_title, name), 19f, color(R.color.text_strong), MEDIUM),
                lp().margins(this, t = 12))
            sheet.addView(label(getString(R.string.server_remove_body), 13.5f, color(R.color.text_muted)), lp().margins(this, t = 6))
            val actions = lin(vertical = false)
            actions.addView(button(getString(R.string.cancel), ButtonKind.SECONDARY) { dialog.dismiss() }, lp(0, WRAP, 1f))
            actions.addView(button(getString(R.string.server_remove_confirm), ButtonKind.DANGER) {
                dialog.dismiss()
                remove()
            }, lp(0, WRAP, 1f).margins(this, l = 10))
            sheet.addView(actions, lp().margins(this, t = 16, b = 10))
        }
    }

    /** Closes the connection, silences and clears its notifications, deletes its local data and keys. */
    private fun remove() {
        Registry.connections.remove(serverId)?.stop()
        app.inbox.alert.Alerter.forgetServer(this, serverId)
        Store.get(this).deleteServer(serverId)
        Registry.forget(serverId)
        InboxService.start(this)
        Toast.makeText(this, getString(R.string.server_removed), Toast.LENGTH_SHORT).show()
        finish()
    }
}
