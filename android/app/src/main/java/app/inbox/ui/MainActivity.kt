package app.inbox.ui

import android.content.Intent
import android.content.res.ColorStateList
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.text.InputType
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.view.Gravity
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.CheckBox
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ScrollView
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import app.inbox.BuildConfig
import app.inbox.R
import app.inbox.alert.Alerter
import app.inbox.data.Alert
import app.inbox.data.Server
import app.inbox.data.Store
import app.inbox.service.LinkState
import app.inbox.service.InboxService
import app.inbox.service.Registry

/** Home: latest alerts, plus the Servers and Settings screens behind the menu. */
class MainActivity : BaseActivity() {
    companion object {
        private const val EXTRA_SERVER = "open_server"
        private const val EXTRA_ALERT = "open_alert"
        private const val EXTRA_NOTICE = "notice"

        /** Opens the home screen on one alert (from its notification); opening marks it read. */
        fun openAlert(context: android.content.Context, serverId: Long, alertId: String): Intent =
            Intent(context, MainActivity::class.java)
                .setAction("app.inbox.OPEN_ALERT:$serverId:$alertId")
                .putExtra(EXTRA_SERVER, serverId)
                .putExtra(EXTRA_ALERT, alertId)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)

        /** Back to the home screen with a snackbar, e.g. "Server deleted". */
        fun notice(context: android.content.Context, text: String): Intent =
            Intent(context, MainActivity::class.java)
                .putExtra(EXTRA_NOTICE, text)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    }

    private enum class Screen { ALERTS, SERVERS, SETTINGS }

    private var screen = Screen.ALERTS
    /** Older alerts loaded with "See more", beyond the configured count and window. */
    private var extra = 0
    private lateinit var root: FrameLayout
    private lateinit var main: LinearLayout
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

        root = FrameLayout(this)
        val behind = FrameLayout(this)
        behind.addView(Backdrop(this), FrameLayout.LayoutParams(MATCH, MATCH))
        main = lin()
        bar = FrameLayout(this)
        main.addView(bar, lp())
        body = lin(padH = 10, padV = 4)
        scroller = scroll(body)
        main.addView(scroller, lp(MATCH, 0, 1f))
        behind.addView(main, FrameLayout.LayoutParams(MATCH, MATCH))
        root.addView(behind, FrameLayout.LayoutParams(MATCH, MATCH))

        val scrim = View(this)
        root.addView(scrim, FrameLayout.LayoutParams(MATCH, MATCH))
        drawer = Drawer(this, scrim, behind)
        drawer.onToggle = { syncBack() }
        drawerContent = lin()
        drawer.addView(drawerContent, lp(MATCH, MATCH))
        root.addView(drawer, FrameLayout.LayoutParams(drawerWidth(), MATCH))

        setContentView(root)
        applyInsets(main)
        applyInsets(drawer)
        InboxService.start(this)
        handleOpen(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleOpen(intent)
    }

    /** A notification was tapped: show that alert (which marks it read). */
    private fun handleOpen(intent: Intent?) {
        intent?.getStringExtra(EXTRA_NOTICE)?.let { text ->
            intent.removeExtra(EXTRA_NOTICE)
            root.post { root.snackbar(text) }
        }
        val serverId = intent?.getLongExtra(EXTRA_SERVER, -1) ?: -1
        val alertId = intent?.getStringExtra(EXTRA_ALERT) ?: return
        intent.removeExtra(EXTRA_ALERT)
        val store = Store.get(this)
        val a = store.alert(serverId, alertId) ?: return
        if (screen != Screen.ALERTS) go(Screen.ALERTS)
        showAlert(a, store.server(serverId)?.serverName ?: "")
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

    /*
     * Back closes the drawer, then returns to the alerts. On Android 13+ the
     * callback is registered only while there is something to close, so Back on
     * the home screen keeps the system's predictive "back to home" animation.
     */
    private val backCallback: Any? = if (Build.VERSION.SDK_INT >= 33) OnBackInvokedCallback { handleBack() } else null
    private var backRegistered = false

    private fun handleBack(): Boolean {
        when {
            drawer.isOpen -> drawer.close()
            screen != Screen.ALERTS -> go(Screen.ALERTS)
            else -> return false
        }
        return true
    }

    private fun syncBack() {
        if (Build.VERSION.SDK_INT < 33) return
        val wanted = drawer.isOpen || screen != Screen.ALERTS
        if (wanted == backRegistered) return
        val cb = backCallback as OnBackInvokedCallback
        if (wanted) onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, cb)
        else onBackInvokedDispatcher.unregisterOnBackInvokedCallback(cb)
        backRegistered = wanted
    }

    /** Android 8 to 12 only: 13+ uses [backCallback]. */
    @Deprecated("Replaced by OnBackInvokedCallback on Android 13+")
    override fun onBackPressed() {
        @Suppress("DEPRECATION")
        if (!handleBack()) super.onBackPressed()
    }

    /** "Marked as read · Undo" for 5 s; Undo makes [alerts] unread again. */
    private fun offerUndo(alerts: List<Pair<Long, String>>) {
        if (alerts.isEmpty()) return
        val text = if (alerts.size == 1) getString(R.string.marked_read)
        else resources.getQuantityString(R.plurals.marked_read_n, alerts.size, alerts.size)
        root.snackbar(text, getString(R.string.undo)) {
            Alerter.markUnread(this, alerts)
            render()
        }
    }

    private fun go(s: Screen) {
        screen = s
        extra = 0
        if (drawer.isOpen) drawer.close()
        scroller.scrollTo(0, 0)
        render()
    }

    // ------------------------------------------------------------ chrome

    private data class Overall(val connected: Int, val total: Int)

    private fun overall(servers: List<Server>): Overall =
        Overall(servers.count { Registry.status(it.id).state == LinkState.CONNECTED }, servers.size)

    /**
     * Status light, no words: all connected = a small blue dot that breathes;
     * degraded = amber dot + "1/2"; all down = red dot + "0/2".
     */
    private fun statusIndicator(o: Overall): View {
        val row = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        val (tint, pulse) = when {
            o.total == 0 -> color(R.color.text_subtle) to false
            o.connected == o.total -> color(R.color.primary) to true
            o.connected > 0 -> color(R.color.warning) to false
            else -> color(R.color.danger) to false
        }
        row.addView(PulseDot(this, tint, pulse), lp(dp(22), dp(22)))
        downCount(o)?.let { row.addView(mono(it, SP_S, tint, bold = true), lp(WRAP, WRAP).margins(this, l = 2)) }
        row.contentDescription = when {
            o.total == 0 -> getString(R.string.status_no_server)
            o.connected == o.total -> getString(R.string.status_all_online)
            else -> resources.getQuantityString(R.plurals.drawer_servers_down, o.total - o.connected, o.total - o.connected)
        }
        return row
    }

    /** "1/2" when some server is down, null when all are up. */
    private fun downCount(o: Overall): String? = if (o.total > 0 && o.connected < o.total) "${o.connected}/${o.total}" else null

    private fun render() {
        val servers = Store.get(this).servers()
        val o = overall(servers)
        title = when (screen) {
            Screen.ALERTS -> getString(R.string.app_name)
            Screen.SERVERS -> getString(R.string.servers_title)
            Screen.SETTINGS -> getString(R.string.settings_title)
        }
        bar.removeAllViews()
        bar.addView(topBar(R.drawable.ic_menu, getString(R.string.nav_open), { drawer.open() }, statusIndicator(o)))
        renderDrawer(servers, o)

        val y = scroller.scrollY
        body.removeAllViews()
        when (screen) {
            Screen.ALERTS -> renderAlerts(servers)
            Screen.SERVERS -> renderServers(servers)
            Screen.SETTINGS -> renderSettings()
        }
        scroller.post { scroller.scrollTo(0, y) }
        syncBack()
    }

    private fun renderDrawer(servers: List<Server>, o: Overall) {
        drawerContent.removeAllViews()
        val head = lin(padH = 16, padV = 16)
        val who = Prefs.defaultUserName(this)
        head.addView(label(getString(R.string.drawer_identity), SP_S, color(R.color.text_muted)))
        head.addView(if (who != null) mono("@$who", SP_L, color(R.color.text_strong), bold = true)
            else label(getString(R.string.drawer_no_pseudo), SP_L, color(R.color.text_muted), MEDIUM), lp().margins(this, t = 4))
        val st = lin(vertical = false)
        st.addView(statusIndicator(o))
        val stText = when {
            o.total == 0 -> getString(R.string.drawer_servers_none)
            o.connected == o.total -> getString(R.string.drawer_servers_all_up)
            else -> resources.getQuantityString(R.plurals.drawer_servers_down, o.total - o.connected, o.total - o.connected)
        }
        st.addView(label(stText, SP_S, color(R.color.text_muted)), lp(WRAP, WRAP).margins(this, l = 8))
        head.addView(st, lp().margins(this, t = 12))
        drawerContent.addView(head)
        drawerContent.addView(divider())

        val store = Store.get(this)
        val unread = store.unreadCount()
        val counts = mapOf(
            Screen.ALERTS to if (unread > 0) unread.toString() else "",
            Screen.SERVERS to (downCount(o) ?: ""),
            Screen.SETTINGS to if (Permissions.missing(this).isEmpty()) "" else "!",
        )
        val items = lin(padH = 8, padV = 8)
        fun item(s: Screen, icon: Int, text: Int) {
            val on = s == screen
            val fg = if (on) color(R.color.primary) else color(R.color.text)
            val row = lin(vertical = false, padH = 12, padV = 0).apply {
                minimumHeight = dp(48)
                background = pressable(if (on) color(R.color.primary_soft) else 0x00000000, dpf(CONTROL_RADIUS))
                isClickable = true
                setOnClickListener { go(s) }
                addView(icon(icon, fg, 18), lp(dp(18), dp(18)).margins(this@MainActivity, r = 14))
                addView(label(getString(text), SP_M, fg, MEDIUM), lp(0, WRAP, 1f))
                val count = counts[s].orEmpty()
                if (count.isNotEmpty()) {
                    val warn = count == "!"
                    val unreadBadge = s == Screen.ALERTS
                    addView(mono(count, SP_S, when {
                        warn -> color(R.color.warning)
                        unreadBadge -> color(R.color.primary)
                        else -> color(R.color.text_muted)
                    }, bold = warn || unreadBadge))
                }
            }
            items.addView(row, lp().margins(this, b = 2))
        }
        item(Screen.ALERTS, R.drawable.ic_bell, R.string.nav_alerts)
        item(Screen.SERVERS, R.drawable.ic_server, R.string.nav_servers)
        item(Screen.SETTINGS, R.drawable.ic_settings, R.string.nav_settings)
        if (unread > 0) {
            items.addView(divider(), lp(MATCH, 1).margins(this, t = 6, b = 6))
            val markAll = lin(vertical = false, padH = 12, padV = 0).apply {
                minimumHeight = dp(48)
                background = pressable(0x00000000, dpf(CONTROL_RADIUS))
                isClickable = true
                contentDescription = getString(R.string.alerts_mark_all)
                setOnClickListener {
                    val marked = Alerter.markAllRead(this@MainActivity)
                    drawer.close()
                    render()
                    offerUndo(marked)
                }
                addView(icon(R.drawable.ic_check_circle, color(R.color.primary), 18), lp(dp(18), dp(18)).margins(this@MainActivity, r = 14))
                addView(label(getString(R.string.alerts_mark_all), SP_M, color(R.color.primary), MEDIUM), lp(0, WRAP, 1f))
            }
            items.addView(markAll)
        }
        drawerContent.addView(items, lp(MATCH, 0, 1f))
        drawerContent.addView(divider())
        drawerContent.addView(mono("v${BuildConfig.VERSION_NAME}", SP_XS, color(R.color.text_subtle)).apply {
            setPadding(dp(16), dp(12), dp(16), dp(14))
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
            }, lp().margins(this, t = 8))
        } else if (missing.isNotEmpty()) {
            body.addView(banner(getString(R.string.alert_permissions_missing), getString(R.string.alert_permissions_fix),
                color(R.color.warning), color(R.color.warning_soft)) { go(Screen.SETTINGS) }, lp().margins(this, t = 8))
        }

        if (servers.isEmpty()) {
            body.addView(emptyState(R.drawable.ic_server, getString(R.string.alerts_no_server_title),
                getString(R.string.alerts_no_server_body), getString(R.string.alerts_add_server)) { addServer() })
            return
        }
        // A server whose groups are known but none followed sends this phone nothing.
        for (s in servers) {
            if (store.subscriptions(s.id).isNotEmpty() || store.groups(s.id).isEmpty()) continue
            body.addView(banner(getString(R.string.alerts_no_group, s.serverName), getString(R.string.alerts_choose_groups),
                color(R.color.primary), color(R.color.primary_soft), R.drawable.ic_bell_outline) { openServer(s.id) },
                lp().margins(this, t = 8))
        }

        val limit = Prefs.limit(this)
        val window = Prefs.windowMs(this)
        val showRead = Prefs.showRead(this)
        val now = System.currentTimeMillis()
        // The configured count within the window, then older ones 10 at a time.
        val base = store.recentAlerts(now - window, limit, showRead)
        val alerts = if (extra == 0) base else store.latestAlerts(base.size + extra, showRead)
        val names = servers.associate { it.id to it.serverName }
        if (alerts.isEmpty()) {
            // The all-clear fills the free space; "Show read" stays pinned at the bottom.
            body.addView(allClear(overall(servers)), lp(MATCH, 0, 1f))
            body.addView(showReadToggle(showRead),
                lp(WRAP, WRAP).margins(this, t = 2, b = 12).apply { gravity = Gravity.CENTER_HORIZONTAL })
            return
        }
        // One card per alert, the severity bar on its left edge. Swipe right: read.
        alerts.forEachIndexed { i, a ->
            val card = lin().apply {
                background = solidCard()
                clipToOutline = true
                outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
                addView(alertRow(a, names[a.serverId] ?: "", now))
            }
            val swipe = SwipeToRead(this, card, enabled = a.unread, removeOnCommit = !showRead) {
                val marked = Alerter.markRead(this, a.serverId, a.alertId)
                render()
                if (marked) offerUndo(listOf(a.serverId to a.alertId))
            }
            body.addView(swipe, lp().margins(this, t = if (i == 0) 4 else 8))
        }
        if (store.alertCount(showRead) > alerts.size) {
            body.addView(seeMore(), lp(WRAP, WRAP).margins(this, t = 2).apply { gravity = Gravity.CENTER_HORIZONTAL })
        }
        body.addView(showReadToggle(showRead),
            lp(WRAP, WRAP).margins(this, t = 2, b = 12).apply { gravity = Gravity.CENTER_HORIZONTAL })
    }

    /** "No alerts" with a check, and how many servers are connected (amber when one is down, red when all are). */
    private fun allClear(o: Overall): View = lin().apply {
        gravity = Gravity.CENTER
        val title = lin(vertical = false).apply { gravity = Gravity.CENTER }
        title.addView(icon(R.drawable.ic_check_circle, color(R.color.primary), 24))
        title.addView(label(getString(R.string.alerts_none), SP_L, color(R.color.text_strong), MEDIUM),
            lp(WRAP, WRAP).margins(this@MainActivity, l = 8))
        addView(title, lp(WRAP, WRAP))
        val up = o.connected == o.total
        val tint = when {
            up -> R.color.text_muted
            o.connected > 0 -> R.color.warning
            else -> R.color.danger
        }
        addView(label(resources.getQuantityString(R.plurals.servers_connected, o.total, o.connected, o.total),
            SP_S, color(tint), if (up) REGULAR else MEDIUM),
            lp(WRAP, WRAP).margins(this@MainActivity, t = 6))
    }

    /** "See more": plain accent text with a 48 dp touch target. */
    private fun seeMore(): View = label(getString(R.string.alerts_see_more), SP_M, color(R.color.primary), MEDIUM).apply {
        gravity = Gravity.CENTER
        minHeight = dp(48)
        setPadding(dp(12), 0, dp(12), 0)
        isClickable = true
        isFocusable = true
        background = rowRipple()
        setOnClickListener {
            extra += 10
            render()
        }
    }

    /** "Show read" native check box, centered alone on its row, identical with or without alerts. */
    private fun showReadToggle(showRead: Boolean): View = CheckBox(this).apply {
        text = getString(R.string.alerts_show_read)
        textSize = SP_M
        typeface = MEDIUM
        isChecked = showRead
        minHeight = dp(48)
        setPadding(dp(6), 0, dp(12), 0)
        setTextColor(color(if (showRead) R.color.primary else R.color.text_muted))
        buttonTintList = ColorStateList(
            arrayOf(intArrayOf(android.R.attr.state_checked), intArrayOf()),
            intArrayOf(color(R.color.primary), color(R.color.text_muted)),
        )
        setOnCheckedChangeListener { _, checked ->
            Prefs.setShowRead(this@MainActivity, checked)
            extra = 0
            render()
        }
    }

    private fun isRinging(a: Alert) = a.action == "alarm" && a.stoppedAtMs == null

    /** Dense event row: severity bar | clock + age | title / group · server — message | tags. */
    private fun alertRow(a: Alert, serverName: String, now: Long): View {
        val style = actionStyle(a.action)
        val ringing = isRinging(a)
        val row = lin(vertical = false).apply {
            minimumHeight = dp(54)
            background = if (ringing) pressable(color(R.color.danger_soft), 0f) else rowRipple()
            isClickable = true
            setOnClickListener { showAlert(a, serverName) }
            // Long press: mark read without opening.
            isLongClickable = true
            setOnLongClickListener {
                if (a.unread && Alerter.markRead(this@MainActivity, a.serverId, a.alertId)) {
                    performHapticFeedback(android.view.HapticFeedbackConstants.LONG_PRESS)
                    render()
                    offerUndo(listOf(a.serverId to a.alertId))
                }
                true
            }
        }
        row.addView(View(this).apply { setBackgroundColor(style.fg) }, lp(dp(3), MATCH))

        val time = lin().apply { setPadding(dp(10), dp(9), 0, dp(9)) }
        time.addView(mono(hms(a.createdAtMs), SP_S, color(R.color.text)))
        time.addView(mono(shortAge(a.createdAtMs, now), SP_XS, color(R.color.text_subtle)), lp().margins(this, t = 3))
        row.addView(time, lp(dp(74), WRAP))

        val content = lin().apply { setPadding(0, dp(9), dp(8), dp(9)) }
        val titleRow = lin(vertical = false)
        if (a.unread) titleRow.addView(statusDot(color(R.color.primary), 6), lp(dp(6), dp(6)).margins(this, r = 6))
        titleRow.addView(label(a.description ?: a.groupName, SP_M,
            if (a.unread) color(R.color.text_strong) else color(R.color.text),
            if (a.unread) BOLD else REGULAR, lines = 1), lp(0, WRAP, 1f))
        content.addView(titleRow)
        val meta = SpannableStringBuilder()
        meta.append("${a.groupName} · $serverName")
        meta.setSpan(ForegroundColorSpan(color(R.color.text_subtle)), 0, meta.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        a.details?.let { d -> app.inbox.md.Markdown.firstLine(d)?.let { meta.append("  ").append(it) } }
        content.addView(label(meta, SP_S, color(R.color.text_muted), lines = 1), lp().margins(this, t = 3))
        row.addView(content, lp(0, WRAP, 1f))

        val tags = lin().apply {
            gravity = Gravity.END or Gravity.CENTER_VERTICAL
            setPadding(0, dp(8), dp(10), dp(8))
        }
        tags.addView(chip(style.code, style.fg, style.bg))
        val late = a.receivedAtMs - a.createdAtMs
        when {
            ringing -> tags.addView(chip(getString(R.string.chip_ringing), color(R.color.danger), color(R.color.danger_soft), dot = true), lp(WRAP, WRAP).margins(this, t = 4))
            late > 5 * 60_000 -> tags.addView(chip("+" + shortAge(a.createdAtMs, a.receivedAtMs), color(R.color.warning), color(R.color.warning_soft)),
                lp(WRAP, WRAP).margins(this, t = 4))
        }
        row.addView(tags, lp(WRAP, MATCH))
        return row
    }

    private fun showAlert(a: Alert, serverName: String) {
        if (isRinging(a)) {
            startActivity(AlarmActivity.intent(this, a.serverId, a.alertId))
            return
        }
        // Opening an alert marks it read (local only).
        if (a.unread) {
            Alerter.markRead(this, a.serverId, a.alertId)
            render()
        }
        val style = actionStyle(a.action)
        bottomSheet { sheet, dialog ->
            val head = lin(vertical = false)
            head.addView(chip(style.code, style.fg, style.bg))
            head.addView(label("${a.groupName} · $serverName", SP_S, color(R.color.text_muted), lines = 1),
                lp(0, WRAP, 1f).margins(this, l = 8))
            head.addView(mono(shortAge(a.createdAtMs), SP_S, color(R.color.text_subtle)))
            sheet.addView(head)
            sheet.addView(label(a.description ?: a.groupName, SP_L, color(R.color.text_strong), BOLD).apply { setTextIsSelectable(true) },
                lp().margins(this, t = 12))
            a.details?.let { sheet.addView(markdown(it), lp().margins(this, t = 10)) }
            val meta = panel(padV = 4)
            val late = a.receivedAtMs - a.createdAtMs
            listOf(
                getString(R.string.meta_server) to serverName,
                getString(R.string.meta_group) to a.groupName,
                getString(R.string.meta_action) to style.code,
                getString(R.string.meta_sent) to formatTime(a.createdAtMs),
                getString(R.string.meta_received) to formatTime(a.receivedAtMs) + if (late > 60_000) "  (+${formatDuration(late)})" else "",
                getString(R.string.meta_read) to formatTime(a.readAtMs ?: System.currentTimeMillis()),
                getString(R.string.meta_alert_id) to a.alertId,
            ).forEachIndexed { i, (k, v) ->
                if (i > 0) meta.addView(divider())
                meta.addView(kv(k, v, if (i == 2) style.fg else color(R.color.text)))
            }
            sheet.addView(meta, lp().margins(this, t = 14))
            sheet.addView(button(getString(R.string.alert_close), ButtonKind.SECONDARY) { dialog.dismiss() }, lp().margins(this, t = 14, b = 10))
        }
    }

    private fun emptyState(iconRes: Int, title: String, text: String, action: String?, onAction: (() -> Unit)?): View {
        val c = panel(padH = 18, padV = 20).apply { gravity = Gravity.CENTER_HORIZONTAL }
        c.addView(iconBadge(iconRes, color(R.color.primary), color(R.color.primary_soft), 40), lp(dp(40), dp(40)))
        c.addView(label(title, SP_L, color(R.color.text_strong), MEDIUM).apply { gravity = Gravity.CENTER }, lp().margins(this, t = 12))
        c.addView(label(text, SP_M, color(R.color.text_muted)).apply { gravity = Gravity.CENTER }, lp().margins(this, t = 4))
        if (action != null && onAction != null) c.addView(button(action) { onAction() }, lp().margins(this, t = 14))
        return lin().apply { addView(c, lp().margins(this@MainActivity, t = 6)) }
    }

    // ------------------------------------------------------------ servers

    private fun addServer() = startActivity(Intent(this, AddServerActivity::class.java))

    private fun openServer(id: Long) =
        startActivity(Intent(this, ServerActivity::class.java).putExtra(ServerActivity.EXTRA_SERVER, id))

    private fun renderServers(servers: List<Server>) {
        val o = overall(servers)
        body.addView(sectionTitle(getString(R.string.servers_title),
            if (servers.isEmpty()) null else downCount(o) ?: getString(R.string.drawer_servers_all_up)))
        if (servers.isEmpty()) {
            body.addView(emptyState(R.drawable.ic_server, getString(R.string.alerts_no_server_title),
                getString(R.string.alerts_no_server_body), null, null))
        } else {
            val now = System.currentTimeMillis()
            val p = panel()
            servers.forEachIndexed { i, s ->
                if (i > 0) p.addView(divider(insetStart = 30))
                val (code, fg) = linkCode(s.id, now)
                val row = lin(vertical = false, padH = 12, padV = 9).apply {
                    minimumHeight = dp(56)
                    background = rowRipple()
                    isClickable = true
                    setOnClickListener { openServer(s.id) }
                }
                row.addView(statusDot(fg, 8), lp(dp(8), dp(8)).margins(this, r = 10))
                val col = lin()
                col.addView(label(s.serverName, SP_M, color(R.color.text_strong), MEDIUM, lines = 1))
                col.addView(label("${s.userName?.let { "@$it" } ?: "—"} · ${s.deviceName}", SP_S, color(R.color.text_muted), lines = 1),
                    lp().margins(this, t = 2))
                col.addView(mono(s.address.removePrefix("http://").removePrefix("https://"), SP_XS, color(R.color.text_subtle)), lp().margins(this, t = 2))
                row.addView(col, lp(0, WRAP, 1f))
                row.addView(label(code, SP_S, fg, MEDIUM, lines = 1), lp(WRAP, WRAP).margins(this, l = 8, r = 4))
                row.addView(icon(R.drawable.ic_chevron_right, color(R.color.text_subtle), 16), lp(dp(16), dp(16)))
                p.addView(row)
            }
            body.addView(p)
        }
        body.addView(button(getString(R.string.servers_add), ButtonKind.SECONDARY) { addServer() }, lp().margins(this, t = 10, b = 16))
    }

    private fun linkCode(serverId: Long, now: Long): Pair<String, Int> {
        val st = Registry.status(serverId)
        return when (st.state) {
            LinkState.CONNECTED -> getString(R.string.status_online) to color(R.color.primary)
            LinkState.CONNECTING -> (st.detail ?: getString(R.string.status_connecting)) to color(R.color.warning)
            LinkState.DISCONNECTED -> getString(R.string.status_offline_since, shortAge(st.sinceMs, now)) to color(R.color.danger)
            LinkState.REENROLL -> getString(R.string.status_reenroll) to color(R.color.danger)
        }
    }

    // ------------------------------------------------------------ settings

    private fun renderSettings() {
        body.addView(sectionTitle(getString(R.string.settings_title)))

        // Default username
        val id = panel(padH = 14, padV = 12)
        id.addView(label(getString(R.string.settings_pseudo), SP_M, color(R.color.text), MEDIUM))
        id.addView(label(getString(R.string.settings_pseudo_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 2))
        val current = Prefs.defaultUserName(this)
        id.addView(pseudoEditor(current) { v ->
            Prefs.setDefaultUserName(this, v)
            render()
            root.snackbar(getString(R.string.saved))
        }, lp().margins(this, t = 10))
        body.addView(id)

        // Alert list
        val d = panel()
        val limit = Prefs.limit(this)
        val stepper = lin(vertical = false).apply {
            background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
            addView(iconButton(R.drawable.ic_minus, getString(R.string.settings_limit_less), color(R.color.text), 16) {
                Prefs.setLimit(this@MainActivity, limit - 1); render()
            }.apply {
                layoutParams = LinearLayout.LayoutParams(dp(44), dp(40))
                isEnabled = limit > Prefs.LIMIT_RANGE.first
                alpha = if (isEnabled) 1f else 0.35f
            })
            addView(mono(limit.toString(), SP_M, color(R.color.text_strong), bold = true).apply {
                gravity = Gravity.CENTER
                minWidth = dp(28)
            })
            addView(iconButton(R.drawable.ic_plus, getString(R.string.settings_limit_more), color(R.color.text), 16) {
                Prefs.setLimit(this@MainActivity, limit + 1); render()
            }.apply {
                layoutParams = LinearLayout.LayoutParams(dp(44), dp(40))
                isEnabled = limit < Prefs.LIMIT_RANGE.last
                alpha = if (isEnabled) 1f else 0.35f
            })
        }
        d.addView(row(getString(R.string.settings_limit), null, stepper))
        d.addView(divider())
        val w = lin(padH = 14, padV = 10)
        w.addView(label(getString(R.string.settings_window), SP_M, color(R.color.text), MEDIUM))
        val window = Prefs.windowMs(this)
        val options = Prefs.WINDOWS_MS
        w.addView(segmented(options.map { windowLabel(it) }, options.indexOf(window).coerceAtLeast(0),
            describe = { i -> getString(R.string.settings_window) + ": " + windowLabel(options[i]) }) { i ->
            Prefs.setWindowMs(this, options[i])
            render()
        }, lp().margins(this, t = 8))
        d.addView(w)
        body.addView(d, lp().margins(this, t = 10))

        // Theme
        val t = lin(padH = 14, padV = 10).apply { background = solidCard() }
        t.addView(label(getString(R.string.settings_theme), SP_M, color(R.color.text), MEDIUM))
        val modes = Prefs.ThemeMode.entries
        val modeNames = listOf(getString(R.string.theme_system), getString(R.string.theme_light), getString(R.string.theme_dark))
        t.addView(segmented(modeNames, modes.indexOf(Prefs.theme(this)),
            describe = { i -> getString(R.string.settings_theme) + ": " + modeNames[i] }) { i ->
            Prefs.setTheme(this, modes[i])
            recreate()
        }, lp().margins(this, t = 8))
        body.addView(t, lp().margins(this, t = 10))

        // Permissions
        val perms = Permissions.all(this)
        val p = panel()
        perms.forEachIndexed { i, perm ->
            if (i > 0) p.addView(divider())
            val trailing = if (perm.granted) chip("OK", color(R.color.primary), color(R.color.primary_soft), dot = true)
            else button(getString(R.string.perm_grant)) { perm.request(this@MainActivity) }
            p.addView(row(getString(perm.title), getString(perm.why), trailing))
        }
        body.addView(p, lp().margins(this, t = 10))

        // About
        val about = panel(padV = 4)
        about.addView(kv(getString(R.string.meta_version), BuildConfig.VERSION_NAME))
        body.addView(about, lp().margins(this, t = 10, b = 20))
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        render()
    }
}

/** Compact single-line input on a field background (monospace for data such as addresses). */
fun android.content.Context.textField(value: String, hint: String, password: Boolean = false, mono: Boolean = false): EditText = EditText(this).apply {
    setText(value)
    this.hint = hint
    isSingleLine = true
    textSize = SP_M
    minHeight = dp(48)
    if (mono) typeface = MONO
    setTextColor(color(R.color.text))
    setHintTextColor(color(R.color.text_subtle))
    background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
    setPadding(dp(12), dp(8), dp(12), dp(8))
    inputType = if (password) InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
    else InputType.TYPE_CLASS_TEXT
    imeOptions = EditorInfo.IME_ACTION_DONE
}

/**
 * Pseudo shown read-only with an "Edit" action. Editing focuses the field and
 * shows Cancel / OK; OK is enabled only once the trimmed value is valid and
 * differs from the current one. Cancel restores the current value.
 */
fun android.content.Context.pseudoEditor(current: String?, onSave: (String) -> Unit): LinearLayout {
    val ctx = this
    val input = textField(current ?: "", getString(R.string.pseudo_hint), mono = true)
    val actions = lin(vertical = false)
    val box = lin(vertical = false).apply {
        addView(input, lp(0, WRAP, 1f))
        addView(actions, lp(WRAP, WRAP).margins(ctx, l = 8))
    }
    fun small(text: String, primary: Boolean, onClick: () -> Unit) =
        label(text, SP_M, color(if (primary) R.color.on_primary else R.color.text), MEDIUM).apply {
            gravity = Gravity.CENTER
            minHeight = dp(48)
            minWidth = dp(48)
            setPadding(dp(14), 0, dp(14), 0)
            isFocusable = true
            background = if (primary) pressable(color(R.color.primary), dpf(CONTROL_RADIUS))
            else pressable(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke))
            isClickable = true
            setOnClickListener { if (isEnabled) onClick() }
        }
    val imm = getSystemService(android.content.Context.INPUT_METHOD_SERVICE) as android.view.inputmethod.InputMethodManager
    var showReadOnly: () -> Unit = {}
    lateinit var ok: android.widget.TextView
    val refreshOk = {
        val v = cleanUserName(input.text.toString())
        val enabled = v != null && v != current
        ok.isEnabled = enabled
        ok.alpha = if (enabled) 1f else 0.4f
    }
    val showEditing = {
        input.isEnabled = true
        input.isFocusableInTouchMode = true
        input.requestFocus()
        input.setSelection(input.text.length)
        imm.showSoftInput(input, android.view.inputmethod.InputMethodManager.SHOW_IMPLICIT)
        // Once the keyboard is up, bring the field into view.
        input.postDelayed({ input.requestRectangleOnScreen(android.graphics.Rect(0, 0, input.width, input.height + dp(64))) }, 350)
        actions.removeAllViews()
        actions.addView(small(getString(R.string.cancel), primary = false) {
            input.setText(current ?: "")
            input.error = null
            showReadOnly()
        })
        ok = small(getString(R.string.save_short), primary = true) {
            val v = cleanUserName(input.text.toString())
            if (v != null && v != current) {
                imm.hideSoftInputFromWindow(input.windowToken, 0)
                onSave(v)
            }
        }
        actions.addView(ok, lp(WRAP, WRAP).margins(ctx, l = 6))
        refreshOk()
    }
    showReadOnly = {
        imm.hideSoftInputFromWindow(input.windowToken, 0)
        input.clearFocus()
        input.isEnabled = false
        actions.removeAllViews()
        actions.addView(small(getString(R.string.edit), primary = false) { showEditing() })
    }
    input.addTextChangedListener(object : android.text.TextWatcher {
        override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
        override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
        override fun afterTextChanged(s: android.text.Editable?) { if (actions.childCount == 2) refreshOk() }
    })
    input.setOnEditorActionListener { _, action, _ ->
        if (action == EditorInfo.IME_ACTION_DONE && actions.childCount == 2 && ok.isEnabled) { ok.performClick(); true } else false
    }
    if (current == null) showEditing() else showReadOnly()
    return box
}
