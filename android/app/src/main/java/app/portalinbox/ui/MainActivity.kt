package app.portalinbox.ui

import android.content.Intent
import android.content.res.ColorStateList
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.text.InputType
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
import app.portalinbox.BuildConfig
import app.portalinbox.R
import app.portalinbox.alert.Alerter
import app.portalinbox.data.AgentActions
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.AgentKey
import app.portalinbox.data.Alert
import app.portalinbox.data.Item
import app.portalinbox.data.LocalRetention
import app.portalinbox.data.Server
import app.portalinbox.data.Store
import app.portalinbox.portal.FilterStrip
import app.portalinbox.portal.HomeCounts
import app.portalinbox.portal.HomeFilter
import app.portalinbox.portal.HomePortal
import app.portalinbox.portal.ItemSort
import app.portalinbox.portal.OfflineSummary
import app.portalinbox.proto.AgentOp
import app.portalinbox.service.AgentNotices
import app.portalinbox.service.LinkState
import app.portalinbox.service.InboxService
import app.portalinbox.service.Items
import app.portalinbox.service.Registry
import app.portalinbox.service.ServerClock
import java.time.ZoneId

/** Home: latest alerts, plus the Servers and Settings screens behind the menu. */
class MainActivity : BaseActivity() {
    companion object {
        private const val EXTRA_SERVER = "open_server"
        private const val EXTRA_ALERT = "open_alert"
        private const val EXTRA_NOTICE = "notice"

        /** Opens the home screen on one alert (from its notification); opening marks it read. */
        fun openAlert(context: android.content.Context, serverId: Long, alertId: String): Intent =
            Intent(context, MainActivity::class.java)
                .setAction("app.portalinbox.OPEN_ALERT:$serverId:$alertId")
                .putExtra(EXTRA_SERVER, serverId)
                .putExtra(EXTRA_ALERT, alertId)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)

        /** Opens the home screen. */
        fun home(context: android.content.Context): Intent =
            Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)

        /** Back to the home screen with a snackbar, e.g. "Server deleted". */
        fun notice(context: android.content.Context, text: String): Intent =
            Intent(context, MainActivity::class.java)
                .putExtra(EXTRA_NOTICE, text)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    }

    private enum class Screen { ALERTS, DONE, SERVERS, SETTINGS }

    private var screen = Screen.ALERTS

    /**
     * The filter of the home list. It is not saved: each start of the app shows All (question Q9), because a saved filter can
     * hide the request of another portal for days.
     */
    private var filter: HomeFilter = HomeFilter.AT_START
    private lateinit var strip: FrameLayout
    private val network = NetworkState(this) { render() }
    /** Older alerts loaded with "See more", beyond the configured count and window. */
    private var extra = 0
    private lateinit var root: FrameLayout
    private lateinit var main: LinearLayout
    private lateinit var bar: FrameLayout
    private lateinit var scroller: ScrollView
    /** Fixed row under the list: always on screen, whatever the list length. */
    private lateinit var footer: FrameLayout
    private lateinit var body: LinearLayout
    private lateinit var drawer: Drawer
    private lateinit var drawerContent: LinearLayout
    private val handler = Handler(Looper.getMainLooper())

    /** The groups of "To answer" that the owner folded. Local to the phone. */
    private var collapse = AgentCollapse()

    /** The names that the owner typed in the cards of the new agents, by portal and agent: a redraw never loses them. */
    private val nameDrafts = HashMap<String, String>()

    /** The owner types in a name field now: the screen does not redraw under the keyboard. The redraw waits for the end of the typing. */
    private var typing = false
    private var redrawAfterTyping = false

    private val onChange: () -> Unit = {
        render()
        showAgentNotices()
    }
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
        strip = FrameLayout(this)
        main.addView(strip, lp())
        body = lin(padH = 10, padV = 4)
        scroller = scroll(body)
        main.addView(scroller, lp(MATCH, 0, 1f))
        footer = FrameLayout(this)
        main.addView(footer, lp())
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
        // The home list from a notification or a link shows everything: a filter must not hide the item that was tapped.
        if (intent.getStringExtra(EXTRA_NOTICE) == null) filter = HomeFilter.AT_START
        handleOpen(intent)
    }

    /** A notification was tapped: show that alert (which marks it read). */
    private fun handleOpen(intent: Intent?) {
        // An update removed servers of protocol 3: tell the person once. Opening the
        // database runs the migration that sets the flag.
        Store.get(this).servers()
        if (Prefs.takeEnrollAgain(this)) {
            root.post { root.snackbar(getString(R.string.enroll_again_notice), durationMs = 20_000) }
        }
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
        showAlert(a, homePortals().firstOrNull { it.id == serverId })
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString("screen", screen.name)
    }

    /** Erases the local copy of the finished requests that are older than the retention of their portal: its own value, else the global one. */
    private fun purgeLocalCopies() {
        val repo = Items.repository(this)
        val global = Prefs.localRetentionDays(this)
        val now = System.currentTimeMillis()
        for (server in Store.get(this).servers()) LocalRetention.purge(repo, server.id, now, LocalRetention.daysFor(global, server.retentionDays))
    }

    override fun onResume() {
        super.onResume()
        collapse = Prefs.agentCollapse(this)
        purgeLocalCopies()
        Registry.addListener(onChange)
        network.start()
        handler.post(tick)
        InboxService.start(this)
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
        network.stop()
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

    /** The portals as the home list draws them: identity, state of the link, "New" mark. */
    private fun homePortals(): List<HomePortal> =
        HomePortal.build(Store.get(this).servers(), { Registry.status(it) }, System.currentTimeMillis())

    /** The clock offset of each portal, for the order of the items across portals. */
    private fun clockOffsets(servers: List<Server>): Map<Long, Long> = servers.associate { it.id to ServerClock.offset(this, it.id) }

    /** The items of the portals that the filter shows. */
    private fun filteredItems(servers: List<Server>): List<Item> = allItems(servers.filter { filter.matches(it.id) })

    /** The strip of filter chips under the top bar. It shows on the alerts screen and on the Done screen. */
    private fun renderStrip(portals: List<HomePortal>) {
        strip.removeAllViews()
        if (screen != Screen.ALERTS && screen != Screen.DONE) return
        val counts = HomeCounts.build(portals.map { it.id }, Items.repository(this).openCounts(), Store.get(this).unreadCounts())
        val chips = FilterStrip.chips(portals, counts, filter)
        if (chips.isEmpty()) return
        strip.addView(filterStrip(chips) { chip ->
            filter = HomeFilter.tap(filter, chip.filter)
            extra = 0
            scroller.scrollTo(0, 0)
            render()
        })
    }

    /** The line of the last `agent_result`, if one waits: a snackbar, once. */
    private fun showAgentNotices() {
        while (true) {
            val line = AgentNotices.take() ?: return
            root.snackbar(line)
        }
    }

    private fun render() {
        // A name field has the focus: wait for the end of the typing. A field that is gone (another screen) never holds the redraw.
        if (typing) {
            if (currentFocus is EditText) {
                redrawAfterTyping = true
                return
            }
            typing = false
        }
        val servers = Store.get(this).servers()
        val portals = homePortals()
        // A filter on a portal that was removed falls back to All.
        filter = filter.resolve(servers.map { it.id })
        val o = overall(servers)
        title = when (screen) {
            Screen.ALERTS -> getString(R.string.nav_inbox)
            Screen.DONE -> getString(R.string.done_title)
            Screen.SERVERS -> getString(R.string.servers_title)
            Screen.SETTINGS -> getString(R.string.settings_title)
        }
        bar.removeAllViews()
        // One dot per portal in its color, hollow when it is down. With no portal, the single status light stays.
        bar.addView(topBar(R.drawable.ic_menu, getString(R.string.nav_open), { drawer.open() },
            if (portals.isEmpty()) statusIndicator(o) else portalDots(portals)))
        renderStrip(portals)
        renderDrawer(servers, o)

        val y = scroller.scrollY
        body.removeAllViews()
        when (screen) {
            Screen.ALERTS -> renderAlerts(servers, portals)
            Screen.DONE -> renderDone(servers, portals)
            Screen.SERVERS -> renderServers(servers)
            Screen.SETTINGS -> renderSettings()
        }
        scroller.post { scroller.scrollTo(0, y) }
        // "Show read" stays pinned under the alerts list, never inside it.
        footer.removeAllViews()
        if (screen == Screen.ALERTS && servers.isNotEmpty()) {
            val doneCount = filteredItems(servers).count { !it.isOpen }
            val row = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
            row.addView(showReadToggle(Prefs.showRead(this)))
            if (doneCount > 0) {
                row.addView(label(getString(R.string.done_link, doneCount), SP_M, color(R.color.primary), MEDIUM).apply {
                    gravity = Gravity.CENTER
                    minHeight = dp(48)
                    setPadding(dp(16), 0, dp(12), 0)
                    isClickable = true
                    isFocusable = true
                    background = rowRipple()
                    setOnClickListener { go(Screen.DONE) }
                })
            }
            footer.addView(row, FrameLayout.LayoutParams(WRAP, WRAP, Gravity.CENTER_HORIZONTAL).apply { bottomMargin = dp(8) })
        }
        syncBack()
    }

    private fun renderDrawer(servers: List<Server>, o: Overall) {
        drawerContent.removeAllViews()
        val head = lin(padH = 16, padV = 16)
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
        item(Screen.ALERTS, R.drawable.ic_bell, R.string.nav_inbox)
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

    private fun renderAlerts(servers: List<Server>, portals: List<HomePortal>) {
        val store = Store.get(this)
        val missing = Permissions.missing(this)
        val byId = portals.associateBy { it.id }
        val nowMs = System.currentTimeMillis()
        // The banners of the portals that cannot be reached, then the banners of the permissions. They are part of the screen,
        // not notifications.
        for (b in OfflineSummary.banners(portals, network.available, nowMs, ZoneId.systemDefault())) {
            body.addView(statusBanner(b) {
                if (b.portalId != null) openServer(b.portalId) else go(Screen.SERVERS)
            }, lp().margins(this, t = 8))
        }
        val wantsAlarm = servers.any { s -> store.levels(s.id).values.any { it == "alarm" } }
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
            body.addView(banner(getString(R.string.alerts_no_group, s.nickname), getString(R.string.alerts_choose_groups),
                color(R.color.primary), color(R.color.primary_soft), R.drawable.ic_bell_outline) { openServer(s.id) },
                lp().margins(this, t = 8))
        }

        val limit = Prefs.limit(this)
        val window = Prefs.windowMs(this)
        val showRead = Prefs.showRead(this)
        val now = System.currentTimeMillis()
        // The configured count within the window, then older ones 10 at a time.
        val only = filter.portalId
        val base = store.recentAlerts(now - window, limit, showRead, only)
        val alerts = if (extra == 0) base else store.latestAlerts(base.size + extra, showRead, only)
        // The items of the portals that the filter shows, the oldest first across portals with the limited time of each portal.
        val shownItems = filteredItems(servers)
        val toAnswer = ItemSort.toAnswer(shownItems, clockOffsets(servers))
        // The agents (lot G4): the groups of "To answer", and the cards of the new agents above them.
        val directory = AgentDirectory(Items.repository(this).allAgents())
        val groups = ItemListOrder.toAnswer(toAnswer, directory, collapse)
        val agentCards = NewAgentCard.build(
            directory, shownItems, { ServerClock.now(this, it) }, ZoneId.systemDefault(),
            pending = { serverId, agentId -> AgentCommands.pending(this, serverId, agentId) }, shows = { filter.matches(it) },
        )
        pruneCollapse(servers)
        val filterName = only?.let { byId[it]?.identity?.nickname }
        // Alice's rule: the alerts come first, then the items to answer. The new agents sit between. A section with no row does not show.
        val sections = ItemListOrder.sections(hasAlerts = alerts.isNotEmpty(), hasItems = groups.isNotEmpty(), hasNewAgents = agentCards.isNotEmpty())
        if (sections.isEmpty()) {
            if (filterName != null) {
                body.addView(label(HomeFilter.nothingFrom(filterName), SP_L, color(R.color.text_muted), MEDIUM).apply { gravity = Gravity.CENTER },
                    lp(MATCH, 0, 1f).margins(this, t = 32))
            } else {
                body.addView(allClear(overall(servers)), lp(MATCH, 0, 1f))
            }
            return
        }
        val titled = ItemListOrder.Section.TO_ANSWER in sections
        for (section in sections) {
            when (section) {
                ItemListOrder.Section.ALERTS -> {
                    if (titled) body.addView(sectionTitle(HomeFilter.sectionTitle(getString(R.string.section_alerts), filterName), alerts.size.toString()))
                    renderAlertCards(alerts, byId, showRead, now)
                    if (store.alertCount(showRead, only) > alerts.size) {
                        body.addView(seeMore(), lp(WRAP, WRAP).margins(this, t = 2).apply { gravity = Gravity.CENTER_HORIZONTAL })
                    }
                }
                ItemListOrder.Section.NEW_AGENTS -> {
                    body.addView(sectionTitle(getString(R.string.agent_new_section), agentCards.size.toString()))
                    agentCards.forEachIndexed { i, m ->
                        val key = AgentKey.of(m.serverId, m.agentId)
                        body.addView(
                            newAgentCard(
                                m, nameDrafts[key] ?: "", { nameDrafts[key] = it },
                                { has ->
                                    typing = has
                                    if (!has && redrawAfterTyping) {
                                        redrawAfterTyping = false
                                        handler.post { render() }
                                    }
                                },
                                agentActions,
                            ),
                            lp().margins(this, t = if (i == 0) 0 else 8),
                        )
                    }
                }
                ItemListOrder.Section.TO_ANSWER -> {
                    val serverNow = HashMap<Long, Long>()
                    fun at(serverId: Long) = serverNow.getOrPut(serverId) { ServerClock.now(this, serverId) }
                    body.addView(sectionTitle(HomeFilter.sectionTitle(getString(R.string.section_to_answer), filterName), toAnswer.size.toString()))
                    groups.forEachIndexed { i, g -> body.addView(agentGroup(g, directory, byId, ::at, now), lp().margins(this, t = if (i == 0) 0 else 8)) }
                }
            }
        }
    }

    /** One card per alert, the severity bar on its left edge. Swipe right: read. */
    private fun renderAlertCards(alerts: List<Alert>, portals: Map<Long, HomePortal>, showRead: Boolean, now: Long) {
        alerts.forEachIndexed { i, a ->
            val card = lin().apply {
                background = solidCard()
                clipToOutline = true
                outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
                addView(alertRow(a, portals[a.serverId], now))
            }
            val swipe = SwipeToRead(this, card, enabled = a.unread, removeOnCommit = !showRead) {
                val marked = Alerter.markRead(this, a.serverId, a.alertId)
                render()
                if (marked) offerUndo(listOf(a.serverId to a.alertId))
            }
            body.addView(swipe, lp().margins(this, t = if (i == 0) 4 else 8))
        }
    }

    private fun allItems(servers: List<Server>): List<Item> {
        val repo = Items.repository(this)
        return servers.flatMap { repo.items(it.id) }
    }

    /** The color of the bar and the chip of an item card: approval in the primary color, question in info, decision grey. */
    private fun chipStyle(chip: ItemListOrder.Chip): Triple<String, Int, Int> = when (chip) {
        ItemListOrder.Chip.APPROVAL -> Triple(getString(R.string.chip_approval), color(R.color.primary), color(R.color.primary_soft))
        ItemListOrder.Chip.QUESTION -> Triple(getString(R.string.chip_question), color(R.color.info), color(R.color.info_soft))
        ItemListOrder.Chip.DECISION -> Triple(getString(R.string.chip_decision), color(R.color.text_muted), color(R.color.bg_hover))
    }

    /** A card of the list "To answer": age and code | title, sender, ages | kind and state chips. */
    private fun itemCard(item: Item, r: ItemListOrder.Row, portal: HomePortal?, nowMs: Long, label: String? = null, framed: Boolean = true): View {
        val (chipText, fgKind, bg) = chipStyle(r.chip)
        // A portal that is down: the row is dimmed and its last line says why. Approve and Decline stay possible when the portal
        // is only offline: the answer waits in the outbox and the portal decides.
        val reason = portal?.let { OfflineSummary.rowReason(it, nowMs) }
        val down = reason != null
        val bar = if (down) color(R.color.text_subtle) else fgKind
        val card = lin().apply {
            // In a group the card of the agent holds the rows: the row has no frame of its own.
            if (framed) {
                background = solidCard()
                clipToOutline = true
                outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
            }
            if (down) alpha = 0.62f
        }
        val row = lin(vertical = false).apply {
            minimumHeight = dp(54)
            background = rowRipple()
            isClickable = true
            // The words of a screen reader: full words, the age and the state as text (TalkBackLabels).
            contentDescription = TalkBackLabels.itemRow(item, ServerClock.now(this@MainActivity, item.serverId),
                portal?.identity?.nickname, reason?.replace(" · ", ". "), label)
            setOnClickListener { openItem(item, r) }
        }
        row.addView(View(this).apply { setBackgroundColor(bar) }, lp(dp(3), MATCH))
        val time = lin().apply { setPadding(dp(10), dp(9), 0, dp(9)) }
        time.addView(mono(r.age, SP_S, color(R.color.text_strong), bold = true))
        r.code?.let { time.addView(mono(it, SP_XS, color(R.color.text_subtle)), lp().margins(this, t = 3)) }
        row.addView(time, lp(dp(74), WRAP))
        val content = lin().apply { setPadding(0, dp(9), dp(8), dp(9)) }
        val titleRow = lin(vertical = false)
        titleRow.addView(statusDot(color(R.color.primary), 6), lp(dp(6), dp(6)).margins(this, r = 6))
        titleRow.addView(label(r.title, SP_M, color(R.color.text_strong), BOLD, lines = 1), lp(0, WRAP, 1f))
        content.addView(titleRow)
        // The portal chip comes first on the second line, before the sender: the nickname is the identity, never the sender text.
        val who = lin(vertical = false)
        portal?.let { who.addView(portalChip(it), lp(WRAP, WRAP).margins(this, r = 6)) }
        who.addView(label(r.subtitle, SP_S, color(R.color.text_muted), lines = 1), lp(0, WRAP, 1f))
        content.addView(who, lp().margins(this, t = 3))
        content.addView(label(reason ?: r.detail, SP_S, if (portal != null && down) reasonColor(portal.health) else color(R.color.text_subtle), lines = 1),
            lp().margins(this, t = 2))
        row.addView(content, lp(0, WRAP, 1f))
        val tags = lin().apply {
            gravity = Gravity.END or Gravity.CENTER_VERTICAL
            setPadding(0, dp(8), dp(10), dp(8))
        }
        tags.addView(chip(chipText, fgKind, bg))
        if (r.old) tags.addView(chip(getString(R.string.chip_old), color(R.color.warning), color(R.color.warning_soft)), lp(WRAP, WRAP).margins(this, t = 4))
        if (r.invalid) tags.addView(chip(getString(R.string.chip_invalid), color(R.color.danger), color(R.color.danger_soft)), lp(WRAP, WRAP).margins(this, t = 4))
        row.addView(tags, lp(WRAP, MATCH))
        card.addView(row)
        return card
    }

    // ------------------------------------------------------------ the agents (lot G4)

    /** Forgets the folded groups that do not exist any more, in all portals: a filter on one portal never forgets the others. */
    private fun pruneCollapse(servers: List<Server>) {
        if (collapse.folded.isEmpty()) return
        val all = ItemSort.toAnswer(allItems(servers), emptyMap())
        val live = AgentGroups.keys(ItemListOrder.toAnswer(all, AgentDirectory(Items.repository(this).allAgents()), AgentCollapse()))
        val kept = collapse.retain(live)
        if (kept !== collapse) {
            collapse = kept
            Prefs.setAgentCollapse(this, kept)
        }
    }

    /** One card for an agent: the header, the hint line, the rows (none when it is folded) and the lock line of a new agent. */
    private fun agentGroup(
        g: AgentGroups.Group, directory: AgentDirectory, portals: Map<Long, HomePortal>, serverNow: (Long) -> Long, nowMs: Long,
    ): View {
        val portal = portals[g.serverId]
        val card = lin().apply {
            background = when {
                g.isNew -> rounded(color(R.color.warning_soft), dpf(CARD_RADIUS), color(R.color.warning) and 0x66FFFFFF, dp(1))
                g.isBucket -> dashedBox(color(R.color.card), color(R.color.card_stroke), dpf(CARD_RADIUS))
                else -> solidCard()
            }
            clipToOutline = true
            outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
        }
        // The portal chip names the portal in the header when several portals exist: the same rule as on each row.
        val chipPortal = portal.takeIf { portals.size > 1 }
        card.addView(agentGroupHeader(g, serverNow(g.serverId), chipPortal, { toggleGroup(g.key) }, { openAgent(g) }))
        g.shown.forEachIndexed { i, item ->
            if (i > 0) card.addView(divider())
            val row = ItemListOrder.row(item, serverNow(item.serverId), directory.labelOf(item), grouped = true)
            card.addView(itemCard(item, row, portal, nowMs, directory.labelOf(item), framed = false))
        }
        if (!g.folded) g.lockLine?.let { card.addView(agentLockLine(it)) }
        return card
    }

    private fun toggleGroup(key: String) {
        collapse = collapse.toggle(key)
        Prefs.setAgentCollapse(this, collapse)
        render()
    }

    private fun openAgent(g: AgentGroups.Group) = startActivity(AgentActivity.intent(this, g.serverId, g.agentId))

    /** Sends an action on an agent. A refusal of the phone shows as a snackbar. The result of the portal comes later as another one. */
    private fun sendAgent(m: NewAgentCard.Model, op: AgentOp, name: String? = null, into: String? = null) {
        val refusal = AgentCommands.run(this, m.serverId, m.agentId, op, name, into)
        if (refusal != null) {
            root.snackbar(refusal)
            return
        }
        if (op == AgentOp.ALLOW) nameDrafts.remove(AgentKey.of(m.serverId, m.agentId))
        render()
    }

    private val agentActions = object : NewAgentActions {
        override fun nameAndAllow(m: NewAgentCard.Model, name: String) {
            when (val c = NewAgentCard.nameAndAllow(name)) {
                is AgentActions.NameCheck.Ok -> sendAgent(m, AgentOp.ALLOW, name = c.name)
                else -> root.snackbar(AgentActions.nameLine(c) ?: "")
            }
        }

        override fun allowAsPortalName(m: NewAgentCard.Model) = sendAgent(m, AgentOp.ALLOW)

        override fun block(m: NewAgentCard.Model) = agentBlockSheet { sendAgent(m, AgentOp.BLOCK) }

        override fun merge(m: NewAgentCard.Model) = agentMergeSheet(m.choices) { into -> sendAgent(m, AgentOp.MERGE, into = into) }
    }

    private fun openItem(item: Item, r: ItemListOrder.Row) {
        if (r.openable) {
            startActivity(ApprovalActivity.intent(this, item.serverId, item.requestId))
        } else {
            // The screens of the questions come with the questions of version 5.2.0.
            root.snackbar(getString(R.string.question_later))
        }
    }

    // ------------------------------------------------------------ done

    private fun renderDone(servers: List<Server>, portals: List<HomePortal>) {
        val byId = portals.associateBy { it.id }
        val done = ItemSort.done(filteredItems(servers), clockOffsets(servers))
        val filterName = filter.portalId?.let { byId[it]?.identity?.nickname }
        body.addView(sectionTitle(HomeFilter.sectionTitle(getString(R.string.done_title), filterName), if (done.isEmpty()) null else done.size.toString()))
        if (done.isEmpty()) {
            body.addView(label(filterName?.let { HomeFilter.nothingFrom(it) } ?: getString(R.string.done_empty), SP_M, color(R.color.text_muted)),
                lp().margins(this, t = 8, l = 4))
            return
        }
        done.forEachIndexed { i, item ->
            val r = ItemListOrder.doneRow(item)
            val (chipText, fg, _) = chipStyle(r.chip)
            val (stateFg, stateBg) = when (r.word) {
                "approved", "answered" -> color(R.color.info) to color(R.color.info_soft)
                "declined" -> color(R.color.danger) to color(R.color.danger_soft)
                "abandoned" -> color(R.color.warning) to color(R.color.warning_soft)
                else -> color(R.color.text_muted) to color(R.color.bg_hover)
            }
            val card = lin().apply {
                background = solidCard()
                clipToOutline = true
                outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
            }
            val row = lin(vertical = false).apply {
                minimumHeight = dp(54)
                background = rowRipple()
                isClickable = true
                contentDescription = listOfNotNull(byId[r.serverId]?.identity?.nickname?.let { "Portal $it" }, chipText, r.title, r.word, r.detail).joinToString(". ")
                setOnClickListener { if (r.openable) startActivity(ApprovalActivity.intent(this@MainActivity, r.serverId, r.requestId)) else root.snackbar(getString(R.string.question_later)) }
            }
            row.addView(View(this).apply { setBackgroundColor(stateFg) }, lp(dp(3), MATCH))
            val time = lin().apply { setPadding(dp(10), dp(9), 0, dp(9)) }
            time.addView(mono(clockTime(r.whenMs), SP_S, color(R.color.text_strong), bold = true))
            row.addView(time, lp(dp(84), WRAP))
            val content = lin().apply { setPadding(0, dp(9), dp(8), dp(9)) }
            content.addView(label(r.title, SP_M, color(R.color.text), REGULAR, lines = 1))
            // The same portal chip as on the cards: a finished item names its portal too.
            byId[r.serverId]?.let { content.addView(portalChip(it.identity, darkTheme(), dashed = false, isNew = it.isNew), lp(WRAP, WRAP).margins(this, t = 3)) }
            content.addView(label(r.detail, SP_S, color(R.color.text_muted), lines = 2), lp().margins(this, t = 3))
            row.addView(content, lp(0, WRAP, 1f))
            val tags = lin().apply {
                gravity = Gravity.END or Gravity.CENTER_VERTICAL
                setPadding(0, dp(8), dp(10), dp(8))
            }
            tags.addView(chip(r.word, stateFg, stateBg))
            row.addView(tags, lp(WRAP, MATCH))
            card.addView(row)
            body.addView(card, lp().margins(this, t = if (i == 0) 4 else 8))
        }
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

    /** "Show read" native check box, in the fixed footer of the alerts screen. */
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
    private fun alertRow(a: Alert, portal: HomePortal?, now: Long): View {
        val style = actionStyle(a.action)
        val ringing = isRinging(a)
        val row = lin(vertical = false).apply {
            minimumHeight = dp(54)
            background = if (ringing) pressable(color(R.color.danger_soft), 0f) else rowRipple()
            isClickable = true
            setOnClickListener { showAlert(a, portal) }
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
        titleRow.addView(label(a.title, SP_M,
            if (a.unread) color(R.color.text_strong) else color(R.color.text),
            if (a.unread) BOLD else REGULAR, lines = 1), lp(0, WRAP, 1f))
        content.addView(titleRow)
        // The portal chip, then the group. The portal name is the nickname of the phone, never the name that the portal sends.
        val meta = lin(vertical = false)
        portal?.let { meta.addView(portalChip(it), lp(WRAP, WRAP).margins(this, r = 6)) }
        meta.addView(label(a.groupName, SP_S, color(R.color.text_muted), lines = 1), lp(0, WRAP, 1f))
        content.addView(meta, lp().margins(this, t = 3))
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

    private fun showAlert(a: Alert, portal: HomePortal?) {
        val serverName = portal?.identity?.nickname ?: ""
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
            portal?.let { head.addView(portalChip(it), lp(WRAP, WRAP).margins(this, l = 8)) }
            head.addView(label(a.groupName, SP_S, color(R.color.text_muted), lines = 1),
                lp(0, WRAP, 1f).margins(this, l = 8))
            head.addView(mono(shortAge(a.createdAtMs), SP_S, color(R.color.text_subtle)))
            sheet.addView(head)
            sheet.addView(label(a.title, SP_L, color(R.color.text_strong), BOLD).apply { setTextIsSelectable(true) },
                lp().margins(this, t = 12))
            val meta = panel(padV = 4)
            val late = a.receivedAtMs - a.createdAtMs
            listOf(
                getString(R.string.meta_server) to serverName,
                getString(R.string.meta_group) to a.groupName,
                getString(R.string.meta_kind) to kindLabel(a.kind),
                getString(R.string.meta_action) to style.code,
                getString(R.string.meta_sent) to formatTime(a.createdAtMs),
                getString(R.string.meta_received) to formatTime(a.receivedAtMs) + if (late > 60_000) "  (+${formatDuration(late)})" else "",
                getString(R.string.meta_read) to formatTime(a.readAtMs ?: System.currentTimeMillis()),
                getString(R.string.meta_message_id) to a.alertId,
            ).forEachIndexed { i, (k, v) ->
                if (i > 0) meta.addView(divider())
                meta.addView(kv(k, v, if (i == 3) style.fg else color(R.color.text)))
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
        startActivity(Intent(this, PortalActivity::class.java).putExtra(PortalActivity.EXTRA_SERVER, id))

    private fun renderServers(servers: List<Server>) = PortalListView.render(this, body, servers, ::openServer, ::addServer, ::render)

    private fun linkCode(serverId: Long, now: Long): Pair<String, Int> {
        val st = Registry.status(serverId)
        return when (st.state) {
            LinkState.CONNECTED -> getString(R.string.status_online) to color(R.color.primary)
            LinkState.CONNECTING -> (st.detail ?: getString(R.string.status_connecting)) to color(R.color.warning)
            LinkState.DISCONNECTED -> getString(R.string.status_offline_since, shortAge(st.sinceMs, now)) to color(R.color.danger)
            LinkState.REENROLL -> getString(R.string.status_reenroll) to color(R.color.danger)
            LinkState.KEY_CHANGED -> getString(R.string.status_key_changed) to color(R.color.danger)
        }
    }

    // ------------------------------------------------------------ settings

    private fun renderSettings() {
        body.addView(sectionTitle(getString(R.string.settings_title)))

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

        // Local copy of the finished requests
        val keep = lin(padH = 14, padV = 10).apply { background = solidCard() }
        keep.addView(label(getString(R.string.settings_local_copy), SP_M, color(R.color.text), MEDIUM))
        keep.addView(label(getString(R.string.settings_local_copy_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 2))
        val days = LocalRetention.OPTIONS_DAYS
        val chosen = Prefs.localRetentionDays(this)
        keep.addView(segmented(days.map { LocalRetention.label(it) }, days.indexOf(chosen).coerceAtLeast(0),
            describe = { i -> getString(R.string.settings_local_copy) + ": " + LocalRetention.label(days[i]) }) { i ->
            Prefs.setLocalRetentionDays(this, days[i])
            purgeLocalCopies()
            render()
        }, lp().margins(this, t = 8))
        body.addView(keep, lp().margins(this, t = 10))

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
fun android.content.Context.textField(value: String, hint: String, mono: Boolean = false): EditText = EditText(this).apply {
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
    inputType = InputType.TYPE_CLASS_TEXT
    imeOptions = EditorInfo.IME_ACTION_DONE
}
