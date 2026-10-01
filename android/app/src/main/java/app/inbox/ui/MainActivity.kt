package app.inbox.ui

import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Toast
import app.inbox.BuildConfig
import app.inbox.R
import app.inbox.data.Alert
import app.inbox.data.Server
import app.inbox.data.Store
import app.inbox.service.LinkState
import app.inbox.service.InboxService
import app.inbox.service.Registry

/** Home: latest alerts, plus the Servers and Settings screens behind the menu. */
class MainActivity : BaseActivity() {
    private enum class Screen { ALERTS, SERVERS, SETTINGS }

    private var screen = Screen.ALERTS
    private lateinit var bar: FrameLayout
    private lateinit var scroller: ScrollView
    private lateinit var body: LinearLayout
    private lateinit var drawer: Drawer
    private lateinit var drawerContent: LinearLayout
    private val handler = Handler(Looper.getMainLooper())
    private val onChange: () -> Unit = { render() }
    private val tick = object : Runnable {
        override fun run() {
            render()
            handler.postDelayed(this, 30_000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        screen = savedInstanceState?.getString("screen")?.let { runCatching { Screen.valueOf(it) }.getOrNull() } ?: Screen.ALERTS

        val root = FrameLayout(this)
        val main = lin()
        bar = FrameLayout(this)
        main.addView(bar, lp())
        body = lin(padH = 16, padV = 8)
        scroller = scroll(body)
        main.addView(scroller, lp(MATCH, 0, 1f))
        root.addView(main, FrameLayout.LayoutParams(MATCH, MATCH))

        val scrim = View(this)
        root.addView(scrim, FrameLayout.LayoutParams(MATCH, MATCH))
        drawer = Drawer(this, scrim)
        drawerContent = lin()
        drawer.addView(drawerContent, lp(MATCH, MATCH))
        root.addView(drawer, FrameLayout.LayoutParams(drawerWidth(), MATCH))

        setContentView(root)
        applyInsets(main)
        applyInsets(drawer)
        InboxService.start(this)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString("screen", screen.name)
    }

    override fun onResume() {
        super.onResume()
        Registry.addListener(onChange)
        handler.post(tick)
        InboxService.start(this)
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
        handler.removeCallbacks(tick)
    }

    @Deprecated("Platform back handling; the app has no AndroidX")
    override fun onBackPressed() {
        when {
            drawer.isOpen -> drawer.close()
            screen != Screen.ALERTS -> go(Screen.ALERTS)
            else -> @Suppress("DEPRECATION") super.onBackPressed()
        }
    }

    private fun go(s: Screen) {
        screen = s
        if (drawer.isOpen) drawer.close()
        scroller.scrollTo(0, 0)
        render()
    }

    // ------------------------------------------------------------ chrome

    private data class Overall(val connected: Int, val total: Int)

    private fun overall(servers: List<Server>): Overall =
        Overall(servers.count { Registry.status(it.id).state == LinkState.CONNECTED }, servers.size)

    private fun statusPill(o: Overall): View = when {
        o.total == 0 -> pill(getString(R.string.status_none), color(R.color.text_muted), color(R.color.bg_hover), dot = true)
        o.connected == o.total -> pill(getString(R.string.status_connected), color(R.color.success), color(R.color.success_soft), dot = true)
        o.connected > 0 -> pill(getString(R.string.status_partial), color(R.color.warning), color(R.color.warning_soft), dot = true)
        else -> pill(getString(R.string.status_offline), color(R.color.danger), color(R.color.danger_soft), dot = true)
    }

    private fun render() {
        val servers = Store.get(this).servers()
        val o = overall(servers)
        val title = when (screen) {
            Screen.ALERTS -> getString(R.string.app_name)
            Screen.SERVERS -> getString(R.string.servers_title)
            Screen.SETTINGS -> getString(R.string.settings_title)
        }
        bar.removeAllViews()
        bar.addView(topBar(title, R.drawable.ic_menu, getString(R.string.nav_open), { drawer.open() }, statusPill(o)))
        renderDrawer(servers, o)

        val y = scroller.scrollY
        body.removeAllViews()
        when (screen) {
            Screen.ALERTS -> renderAlerts(servers)
            Screen.SERVERS -> renderServers(servers)
            Screen.SETTINGS -> renderSettings()
        }
        scroller.post { scroller.scrollTo(0, y) }
    }

    private fun renderDrawer(servers: List<Server>, o: Overall) {
        drawerContent.removeAllViews()
        val head = lin(padH = 20, padV = 24)
        head.addView(iconBadge(R.drawable.ic_bell_ring, color(R.color.primary), color(R.color.primary_soft), 48))
        head.addView(label(getString(R.string.app_name), 22f, color(R.color.text_strong), MEDIUM), lp().margins(this, t = 14))
        val who = Prefs.defaultUserName(this)
        head.addView(label(who ?: getString(R.string.drawer_tagline), 14f, color(R.color.text_muted)), lp().margins(this, t = 4))
        val st = if (o.total == 0) getString(R.string.drawer_servers_none)
        else getString(R.string.drawer_servers_status, o.connected, o.total)
        head.addView(statusPill(o).also { (it as android.widget.TextView).text = st }, lp(WRAP, WRAP).margins(this, t = 12))
        drawerContent.addView(head)
        drawerContent.addView(divider())

        val items = lin(padH = 12, padV = 12)
        fun item(s: Screen, icon: Int, text: Int) {
            val on = s == screen
            val fg = if (on) color(R.color.primary) else color(R.color.text)
            val row = lin(vertical = false, padH = 14, padV = 0).apply {
                minimumHeight = dp(48)
                background = pressable(if (on) color(R.color.primary_soft) else 0x00000000, dpf(9f))
                isClickable = true
                setOnClickListener { go(s) }
                addView(icon(icon, fg, 22), lp(dp(22), dp(22)).margins(this@MainActivity, r = 16))
                addView(label(getString(text), 15f, fg, MEDIUM))
            }
            items.addView(row, lp().margins(this, b = 4))
        }
        item(Screen.ALERTS, R.drawable.ic_bell, R.string.nav_alerts)
        item(Screen.SERVERS, R.drawable.ic_server, R.string.nav_servers)
        item(Screen.SETTINGS, R.drawable.ic_settings, R.string.nav_settings)
        drawerContent.addView(items, lp(MATCH, 0, 1f))
        drawerContent.addView(label(getString(R.string.settings_version, BuildConfig.VERSION_NAME), 12f, color(R.color.text_subtle)).apply {
            setPadding(dp(24), dp(12), dp(24), dp(20))
        })
    }

    // ------------------------------------------------------------ alerts

    private fun renderAlerts(servers: List<Server>) {
        val store = Store.get(this)
        val missing = Permissions.missing(this)
        val wantsAlarm = servers.any { s -> store.subscriptions(s.id).values.any { it == "alarm" } }
        if (wantsAlarm && !Settings.canDrawOverlays(this)) {
            body.addView(banner(getString(R.string.alert_overlay_missing), getString(R.string.alert_overlay_fix),
                color(R.color.danger), color(R.color.danger_soft)) {
                missing.firstOrNull { it.key == "overlay" }?.request?.invoke(this)
            }, lp().margins(this, t = 8, b = 4))
        } else if (missing.isNotEmpty()) {
            body.addView(banner(getString(R.string.alert_permissions_missing), getString(R.string.alert_permissions_fix),
                color(R.color.warning), color(R.color.warning_soft)) { go(Screen.SETTINGS) }, lp().margins(this, t = 8, b = 4))
        }

        if (servers.isEmpty()) {
            body.addView(emptyState(R.drawable.ic_server, getString(R.string.alerts_no_server_title),
                getString(R.string.alerts_no_server_body), getString(R.string.alerts_add_server)) { addServer() })
            return
        }

        val limit = Prefs.limit(this)
        val window = Prefs.windowMs(this)
        val now = System.currentTimeMillis()
        val alerts = store.recentAlerts(now - window, limit)
        val names = servers.associate { it.id to it.serverName }
        body.addView(sectionLabel(getString(R.string.alerts_title)))
        if (alerts.isEmpty()) {
            body.addView(emptyState(R.drawable.ic_check_circle, getString(R.string.alerts_empty_title, windowLabel(window)),
                getString(R.string.alerts_empty_body), null, null))
        }
        for (a in alerts) body.addView(alertCard(a, names[a.serverId] ?: "", now), lp().margins(this, b = 10))

        val footer = lin(vertical = false).apply { gravity = Gravity.CENTER }
        footer.addView(label(getString(R.string.alerts_footer, limit, windowLabel(window)), 13f, color(R.color.text_muted)))
        footer.addView(label(getString(R.string.alerts_footer_edit), 13f).asLink().apply {
            setPadding(dp(4), dp(8), 0, dp(8))
            setOnClickListener { go(Screen.SETTINGS) }
        })
        body.addView(footer, lp().margins(this, t = 6, b = 16))
    }

    private fun isRinging(a: Alert) = a.action == "alarm" && a.stoppedAtMs == null

    private fun alertCard(a: Alert, serverName: String, now: Long): View {
        val style = actionStyle(a.action)
        val c = card(14).apply {
            orientation = LinearLayout.HORIZONTAL
            background = pressable(color(R.color.bg_panel), dpf(12f), if (isRinging(a)) color(R.color.danger) else color(R.color.border))
            isClickable = true
            setOnClickListener { showAlert(a, serverName) }
        }
        c.addView(iconBadge(style.icon, style.fg, style.bg, 40), lp(dp(40), dp(40)).margins(this, r = 12))
        val col = lin()
        val meta = lin(vertical = false)
        meta.addView(label(a.groupName + " · " + serverName, 12f, color(R.color.text_muted), MEDIUM, lines = 1), lp(0, WRAP, 1f))
        meta.addView(label(relativeTime(a.createdAtMs, now), 12f, color(R.color.text_subtle)), lp(WRAP, WRAP).margins(this, l = 8))
        col.addView(meta)
        col.addView(label(a.title ?: a.groupName, 16f, color(R.color.text_strong), MEDIUM, lines = 2), lp().margins(this, t = 4))
        a.message?.let { col.addView(label(it, 14f, color(R.color.text), lines = 2), lp().margins(this, t = 4)) }
        val chips = lin(vertical = false)
        chips.addView(pill(style.label, style.fg, style.bg))
        if (isRinging(a)) chips.addView(pill(getString(R.string.alert_ringing), color(R.color.danger), color(R.color.danger_soft), dot = true), lp(WRAP, WRAP).margins(this, l = 6))
        val late = a.receivedAtMs - a.createdAtMs
        if (late > 5 * 60_000) chips.addView(pill(getString(R.string.alert_late, formatDuration(late)), color(R.color.warning), color(R.color.warning_soft)), lp(WRAP, WRAP).margins(this, l = 6))
        col.addView(chips, lp().margins(this, t = 10))
        c.addView(col, lp(0, WRAP, 1f))
        return c
    }

    private fun showAlert(a: Alert, serverName: String) {
        if (isRinging(a)) {
            startActivity(AlarmActivity.intent(this, a.serverId, a.alertId))
            return
        }
        val style = actionStyle(a.action)
        bottomSheet { sheet, dialog ->
            val head = lin(vertical = false)
            head.addView(iconBadge(style.icon, style.fg, style.bg, 40), lp(dp(40), dp(40)).margins(this, r = 12))
            val t = lin()
            t.addView(label(a.groupName, 14f, color(R.color.text), MEDIUM))
            t.addView(label(serverName, 13f, color(R.color.text_muted)), lp().margins(this, t = 2))
            head.addView(t, lp(0, WRAP, 1f))
            head.addView(pill(style.label, style.fg, style.bg))
            sheet.addView(head)
            sheet.addView(label(a.title ?: a.groupName, 20f, color(R.color.text_strong), MEDIUM), lp().margins(this, t = 18))
            a.message?.let {
                sheet.addView(label(it, 15f, color(R.color.text)).apply { setTextIsSelectable(true) }, lp().margins(this, t = 10))
            }
            sheet.addView(label(getString(R.string.alert_sent_at, formatTime(a.createdAtMs)), 13f, color(R.color.text_muted)), lp().margins(this, t = 18))
            sheet.addView(label(getString(R.string.alert_received_at, formatTime(a.receivedAtMs)), 13f, color(R.color.text_muted)), lp().margins(this, t = 4))
            sheet.addView(button(getString(R.string.alert_close), ButtonKind.SECONDARY) { dialog.dismiss() }, lp().margins(this, t = 20, b = 12))
        }
    }

    private fun emptyState(iconRes: Int, title: String, text: String, action: String?, onAction: (() -> Unit)?): View {
        val c = card(24).apply { gravity = Gravity.CENTER_HORIZONTAL }
        c.addView(iconBadge(iconRes, color(R.color.primary), color(R.color.primary_soft), 64), lp(dp(64), dp(64)).margins(this, t = 8))
        c.addView(label(title, 17f, color(R.color.text_strong), MEDIUM).apply { gravity = Gravity.CENTER }, lp().margins(this, t = 16))
        c.addView(label(text, 14f, color(R.color.text_muted)).apply { gravity = Gravity.CENTER }, lp().margins(this, t = 6))
        if (action != null && onAction != null) c.addView(button(action) { onAction() }, lp().margins(this, t = 20))
        return lin().apply { addView(c, lp().margins(this@MainActivity, t = 8)) }
    }

    // ------------------------------------------------------------ servers

    private fun addServer() {
        if (Prefs.defaultUserName(this) == null) {
            Toast.makeText(this, getString(R.string.add_pseudo_required), Toast.LENGTH_LONG).show()
            go(Screen.SETTINGS)
            return
        }
        startActivity(Intent(this, AddServerActivity::class.java))
    }

    private fun renderServers(servers: List<Server>) {
        body.addView(View(this), lp(MATCH, dp(8)))
        if (servers.isEmpty()) {
            body.addView(label(getString(R.string.servers_empty), 14f, color(R.color.text_muted)).apply { setPadding(dp(4), 0, 0, dp(12)) })
        }
        val now = System.currentTimeMillis()
        for (s in servers) {
            val st = Registry.status(s.id)
            val (text, fg, bg) = when (st.state) {
                LinkState.CONNECTED -> Triple(getString(R.string.status_connected), color(R.color.success), color(R.color.success_soft))
                LinkState.CONNECTING -> Triple(getString(R.string.status_connecting), color(R.color.warning), color(R.color.warning_soft))
                LinkState.DISCONNECTED -> Triple(getString(R.string.status_disconnected_since, formatSince(now - st.sinceMs)), color(R.color.danger), color(R.color.danger_soft))
                LinkState.REENROLL -> Triple(getString(R.string.status_reenroll), color(R.color.danger), color(R.color.danger_soft))
            }
            val c = card(14).apply {
                orientation = LinearLayout.HORIZONTAL
                background = pressable(color(R.color.bg_panel), dpf(12f), color(R.color.border))
                isClickable = true
                setOnClickListener {
                    startActivity(Intent(this@MainActivity, ServerActivity::class.java).putExtra(ServerActivity.EXTRA_SERVER, s.id))
                }
            }
            c.addView(iconBadge(R.drawable.ic_server, color(R.color.primary), color(R.color.primary_soft), 40), lp(dp(40), dp(40)).margins(this, r = 12))
            val col = lin()
            col.addView(label(s.serverName, 16f, color(R.color.text_strong), MEDIUM, lines = 1))
            col.addView(label(s.address, 13f, color(R.color.text_muted), lines = 1), lp().margins(this, t = 2))
            col.addView(label(getString(R.string.server_identity, s.userName ?: getString(R.string.server_no_pseudo), s.deviceName), 13f, color(R.color.text_muted), lines = 1), lp().margins(this, t = 2))
            col.addView(pill(text, fg, bg, dot = true), lp(WRAP, WRAP).margins(this, t = 10))
            c.addView(col, lp(0, WRAP, 1f))
            c.addView(icon(R.drawable.ic_chevron_right, color(R.color.text_subtle), 22), lp(dp(22), dp(22)).apply { gravity = Gravity.CENTER_VERTICAL })
            body.addView(c, lp().margins(this, b = 10))
        }
        body.addView(button("+  " + getString(R.string.servers_add)) { addServer() }, lp().margins(this, t = 6, b = 16))
    }

    // ------------------------------------------------------------ settings

    private fun renderSettings() {
        // Identity
        body.addView(sectionLabel(getString(R.string.settings_identity)))
        val id = card(16)
        id.addView(label(getString(R.string.settings_pseudo), 15f, color(R.color.text), MEDIUM))
        id.addView(label(getString(R.string.settings_pseudo_help), 13f, color(R.color.text_muted)), lp().margins(this, t = 4))
        val current = Prefs.defaultUserName(this)
        val input = textField(current ?: "", getString(R.string.pseudo_hint))
        id.addView(input, lp().margins(this, t = 12))
        if (current == null) {
            id.addView(label(getString(R.string.settings_pseudo_missing), 13f, color(R.color.danger)), lp().margins(this, t = 8))
        }
        val save = {
            val v = cleanUserName(input.text.toString())
            if (v == null) {
                input.error = getString(R.string.pseudo_invalid)
            } else {
                Prefs.setDefaultUserName(this, v)
                // Servers enrolled before pseudos existed adopt the default.
                val store = Store.get(this)
                for (s in store.servers().filter { it.userName == null }) {
                    store.setUserName(s.id, v)
                    Registry.connections[s.id]?.setUserName(v)
                }
                Toast.makeText(this, getString(R.string.saved), Toast.LENGTH_SHORT).show()
                render()
            }
        }
        input.setOnEditorActionListener { _, action, _ -> if (action == EditorInfo.IME_ACTION_DONE) { save(); true } else false }
        id.addView(button(getString(R.string.save), ButtonKind.SECONDARY) { save() }, lp().margins(this, t = 12))
        body.addView(id)

        // Display
        body.addView(sectionLabel(getString(R.string.settings_display)))
        val d = card(0)
        val limit = Prefs.limit(this)
        val stepper = lin(vertical = false).apply {
            addView(iconButton(R.drawable.ic_minus, "-", color(R.color.text)) { Prefs.setLimit(this@MainActivity, limit - 1); render() }.apply {
                background = pressable(color(R.color.bg_subtle), dpf(9f), color(R.color.border))
                isEnabled = limit > Prefs.LIMIT_RANGE.first
                alpha = if (isEnabled) 1f else 0.4f
            })
            addView(label(limit.toString(), 17f, color(R.color.text_strong), MEDIUM).apply {
                gravity = Gravity.CENTER
                minWidth = dp(44)
            })
            addView(iconButton(R.drawable.ic_plus, "+", color(R.color.text)) { Prefs.setLimit(this@MainActivity, limit + 1); render() }.apply {
                background = pressable(color(R.color.bg_subtle), dpf(9f), color(R.color.border))
                isEnabled = limit < Prefs.LIMIT_RANGE.last
                alpha = if (isEnabled) 1f else 0.4f
            })
        }
        d.addView(row(getString(R.string.settings_limit), null, stepper))
        d.addView(divider())
        val w = lin(padH = 16, padV = 14)
        w.addView(label(getString(R.string.settings_window), 15f, color(R.color.text), MEDIUM))
        val window = Prefs.windowMs(this)
        val options = Prefs.WINDOWS_MS
        w.addView(segmented(options.map { windowLabel(it) }, options.indexOf(window).coerceAtLeast(0)) { i ->
            Prefs.setWindowMs(this, options[i])
            render()
        }, lp().margins(this, t = 12))
        d.addView(w)
        body.addView(d)

        // Theme
        body.addView(sectionLabel(getString(R.string.settings_theme)))
        val modes = Prefs.ThemeMode.entries
        body.addView(segmented(
            listOf(getString(R.string.theme_system), getString(R.string.theme_light), getString(R.string.theme_dark)),
            modes.indexOf(Prefs.theme(this)),
        ) { i ->
            Prefs.setTheme(this, modes[i])
            recreate()
        })

        // Permissions
        body.addView(sectionLabel(getString(R.string.settings_permissions)))
        val p = card(0)
        Permissions.all(this).forEachIndexed { i, perm ->
            if (i > 0) p.addView(divider())
            val trailing = if (perm.granted) pill(getString(R.string.perm_granted), color(R.color.success), color(R.color.success_soft), dot = true)
            else label(getString(R.string.perm_grant), 14f, color(R.color.on_primary), MEDIUM).apply {
                background = pressable(color(R.color.primary), dpf(9f))
                setPadding(dp(14), dp(9), dp(14), dp(9))
                isClickable = true
                setOnClickListener { perm.request(this@MainActivity) }
            }
            p.addView(row(getString(perm.title), perm.why, trailing))
        }
        body.addView(p)

        // About
        body.addView(sectionLabel(getString(R.string.settings_about)))
        val about = card(16)
        about.addView(label(getString(R.string.settings_version, BuildConfig.VERSION_NAME), 15f, color(R.color.text), MEDIUM))
        about.addView(label(getString(R.string.settings_about_body), 13f, color(R.color.text_muted)), lp().margins(this, t = 4))
        body.addView(about, lp().margins(this, b = 24))
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        render()
    }
}

/** Single-line input styled like the S3-Browser fields. */
fun android.content.Context.textField(value: String, hint: String, password: Boolean = false): EditText = EditText(this).apply {
    setText(value)
    this.hint = hint
    isSingleLine = true
    textSize = 15f
    setTextColor(color(R.color.text))
    setHintTextColor(color(R.color.text_subtle))
    background = rounded(color(R.color.bg_panel), dpf(9f), color(R.color.border_strong), dp(1))
    setPadding(dp(12), dp(12), dp(12), dp(12))
    inputType = if (password) InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
    else InputType.TYPE_CLASS_TEXT
    imeOptions = EditorInfo.IME_ACTION_DONE
}
