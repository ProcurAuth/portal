package app.portalinbox.ui

import android.os.Bundle
import android.widget.LinearLayout
import app.portalinbox.R
import app.portalinbox.data.Store
import app.portalinbox.service.LinkState
import app.portalinbox.service.InboxService
import app.portalinbox.service.Registry

/** One server: status, account login, and what each group does on this phone. */
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

    /** "Connection details" expanded (kept across rebuilds and rotation). */
    private var detailsOpen = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        detailsOpen = savedInstanceState?.getBoolean("details") ?: false
        val root = lin()
        val server = Store.get(this).server(serverId) ?: return finish()
        title = server.serverName
        bar = android.widget.FrameLayout(this)
        root.addView(bar, lp())
        body = lin(padH = 10, padV = 4)
        root.addView(scroll(body), lp(MATCH, 0, 1f))
        setScreen(root)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean("details", detailsOpen)
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

        val st = Registry.status(serverId)
        val now = System.currentTimeMillis()
        val (code, fg) = when (st.state) {
            LinkState.CONNECTED -> getString(R.string.status_online) to color(R.color.primary)
            LinkState.CONNECTING -> (st.detail ?: getString(R.string.status_connecting)) to color(R.color.warning)
            LinkState.DISCONNECTED -> getString(R.string.status_offline_since, shortAge(st.sinceMs, now)) to color(R.color.danger)
            LinkState.REENROLL -> getString(R.string.status_reenroll) to color(R.color.danger)
        }
        bar.removeAllViews()
        // Same status light as the home; the words are under the server name.
        val light = PulseDot(this, fg, st.state == LinkState.CONNECTED).apply { contentDescription = code }
        bar.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }, light))

        // Which server, where, and its state.
        body.addView(sectionTitle(server.serverName))
        val where = lin(vertical = false).apply { setPadding(dp(4), 0, dp(4), 0) }
        where.addView(label(code, SP_S, fg, MEDIUM, lines = 1))
        where.addView(label(" · ", SP_S, color(R.color.text_muted)))
        where.addView(mono(server.address.removePrefix("http://").removePrefix("https://"), SP_S, color(R.color.text_muted)),
            lp(0, WRAP, 1f))
        body.addView(where)

        renderGroups(store)

        // Login of the account: assigned by the server, read-only.
        body.addView(sectionTitle(getString(R.string.server_account)))
        val p = panel(padH = 14, padV = 12)
        p.addView(mono("@${server.userName}", SP_L, color(R.color.text_strong), bold = true))
        p.addView(label(getString(R.string.server_account_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4))
        body.addView(p)

        // Connection details, collapsed by default: live link values and identity.
        val details = panel()
        val header = lin(vertical = false, padH = 14, padV = 0).apply {
            minimumHeight = dp(48)
            background = rowRipple()
            isClickable = true
            isFocusable = true
            contentDescription = getString(R.string.server_details)
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                stateDescription = getString(if (detailsOpen) R.string.a11y_expanded else R.string.a11y_collapsed)
            }
            setOnClickListener {
                detailsOpen = !detailsOpen
                render()
            }
            addView(label(getString(R.string.server_details), SP_M, color(R.color.text), MEDIUM), lp(0, WRAP, 1f))
            addView(icon(R.drawable.ic_chevron_right, color(R.color.text_muted), 18).apply {
                rotation = if (detailsOpen) 270f else 90f
            }, lp(dp(18), dp(18)))
        }
        details.addView(header)
        pingValue = null
        updateValue = null
        sinceValue = null
        if (detailsOpen) {
            fun add(key: Int, value: String, color: Int = color(R.color.text), lines: Int = 1, mono: Boolean = true): android.widget.TextView {
                details.addView(divider())
                val row = kv(getString(key), value, color, lines, mono)
                details.addView(row)
                return row.getChildAt(row.childCount - 1) as android.widget.TextView
            }
            add(R.string.meta_state, code, fg, mono = false)
            if (st.state != LinkState.CONNECTED && st.detail != null && st.detail != code) {
                add(R.string.meta_detail, st.detail, color(R.color.text_muted), lines = 3, mono = false)
            }
            pingValue = add(R.string.server_last_ping, "—")
            updateValue = add(R.string.server_last_update, "—")
            sinceValue = add(R.string.server_connected_since, "—")
            add(R.string.meta_address, server.address)
            add(R.string.meta_device, server.deviceName)
            add(R.string.meta_device_id, server.deviceId.take(16) + "…")
            refreshLive()
        }
        body.addView(details, lp().margins(this, t = 16))

        body.addView(button(getString(R.string.server_remove), ButtonKind.DANGER) { confirmRemove(server.serverName) },
            lp().margins(this, t = 16, b = 20))
    }

    /** One compact row per group: name and description, then the level for this phone. */
    private fun renderGroups(store: Store) {
        val subs = store.subscriptions(serverId) // local levels; the server only knows the group ids
        val groups = store.groups(serverId)
        body.addView(sectionTitle(getString(R.string.server_groups), if (groups.isEmpty()) null else
            getString(R.string.server_groups_subscribed, groups.count { subs[it.id] != null }, groups.size)))
        if (groups.isEmpty()) {
            body.addView(label(getString(R.string.server_groups_none), SP_M, color(R.color.text_muted)).apply {
                setPadding(dp(4), 0, dp(4), dp(4))
            })
            return
        }
        body.addView(label(getString(R.string.server_groups_hint), SP_S, color(R.color.text_muted)).apply {
            setPadding(dp(4), 0, dp(4), dp(10))
        })
        val labels = listOf(getString(R.string.seg_off), getString(R.string.seg_notify), getString(R.string.seg_ring), getString(R.string.seg_alarm))
        val names = listOf(getString(R.string.action_off), getString(R.string.action_notify), getString(R.string.action_ring), getString(R.string.action_alarm))
        // Side by side when there is room, else the level control goes under the name.
        val inline = resources.configuration.screenWidthDp >= 380 && resources.configuration.fontScale < 1.3f
        val list = panel()
        groups.forEachIndexed { i, g ->
            if (i > 0) list.addView(divider())
            val current = ACTIONS.indexOf(subs[g.id]).coerceAtLeast(0)
            val style = actionStyle(ACTIONS[current])
            val row = lin(vertical = false)
            row.addView(android.view.View(this).apply { setBackgroundColor(if (current == 0) 0x00000000 else style.fg) }, lp(dp(3), MATCH))
            val content = lin(vertical = !inline, padH = 0, padV = 0).apply { setPadding(dp(11), dp(8), dp(8), dp(8)) }
            val text = lin()
            text.addView(label(g.name, SP_M, color(R.color.text_strong), MEDIUM, lines = 2))
            if (g.description.isNotEmpty()) {
                text.addView(label(g.description, SP_S, color(R.color.text_muted), lines = 2), lp().margins(this, t = 2))
            } else if (g.name != g.id) {
                text.addView(mono(g.id, SP_XS, color(R.color.text_subtle)), lp().margins(this, t = 2))
            }
            content.addView(text, if (inline) lp(0, WRAP, 1f).margins(this, r = 8) else lp())
            val seg = segmented(labels, current,
                accent = { k -> actionStyle(ACTIONS[k]).let { it.fg to it.bg } },
                describe = { k -> "${g.name}: ${names[k]}" }) { k ->
                // Saved on the phone right away; the server learns the subscription when connected.
                val conn = Registry.connections[serverId]
                if (conn != null) conn.setLevel(g.id, ACTIONS[k]) else {
                    store.setLevel(serverId, g.id, ACTIONS[k])
                    render()
                }
            }
            content.addView(seg, if (inline) lp(dp(216), WRAP) else lp().margins(this, t = 8))
            row.addView(content, lp(0, WRAP, 1f))
            list.addView(row)
        }
        body.addView(list)
    }

    private fun confirmRemove(name: String) {
        bottomSheet { sheet, dialog ->
            sheet.addView(chip(getString(R.string.server_remove_confirm), color(R.color.danger), color(R.color.danger_soft), dot = true),
                lp(WRAP, WRAP))
            sheet.addView(label(getString(R.string.server_remove_title, name), SP_L, color(R.color.text_strong), BOLD),
                lp().margins(this, t = 12))
            sheet.addView(label(getString(R.string.server_remove_body), SP_M, color(R.color.text_muted)), lp().margins(this, t = 6))
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
        app.portalinbox.alert.Alerter.forgetServer(this, serverId)
        Store.get(this).deleteServer(serverId)
        Registry.forget(serverId)
        InboxService.start(this)
        startActivity(MainActivity.notice(this, getString(R.string.server_removed)))
        finish()
    }
}
