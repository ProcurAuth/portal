package app.portalinbox.ui

import android.content.Intent
import android.os.Bundle
import android.text.InputType
import android.view.View
import android.widget.LinearLayout
import app.portalinbox.R
import app.portalinbox.data.Levels
import app.portalinbox.data.Server
import app.portalinbox.data.Store
import app.portalinbox.portal.AddressChange
import app.portalinbox.portal.FingerprintText
import app.portalinbox.portal.KeyChangedModel
import app.portalinbox.portal.KnownPortal
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PortalNaming
import app.portalinbox.portal.PortalPalette
import app.portalinbox.portal.PortalRow
import app.portalinbox.portal.RemovalSummary
import app.portalinbox.service.LinkState
import app.portalinbox.service.InboxService
import app.portalinbox.service.Registry

/**
 * One portal: its identity on this phone (name, color, monogram, fingerprint), its state, the key-changed block, the groups, the
 * account login, the connection details, and the actions: rename, color, change address, enroll again and remove. The settings
 * (mute, lock screen, quiet hours, summary, retention) are on their own screen: [PortalPrefsActivity]. The name and the color are set here and nowhere else (proposal 3.4).
 */
class PortalActivity : BaseActivity() {
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
        title = server.nickname
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
            LinkState.KEY_CHANGED -> getString(R.string.status_key_changed) to color(R.color.danger)
        }
        bar.removeAllViews()
        // Same status light as the home; the words are under the server name.
        val light = PulseDot(this, fg, st.state == LinkState.CONNECTED).apply { contentDescription = code }
        bar.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }, light))

        // Which portal: monogram, name in the portal color, host and login.
        val identity = PortalIdentity.of(server)
        val portalRow = PortalRow.of(server, st, 0, now)
        val head = lin(vertical = false, padH = 12, padV = 10).apply {
            background = rounded(portalColor(identity) and 0x00FFFFFF or 0x1F000000, dpf(CARD_RADIUS), portalColor(identity) and 0x00FFFFFF or 0x66000000, dp(1))
            addView(monogramSquare(identity, 40), lp(dp(40), dp(40)).margins(this@PortalActivity, r = 12))
            val col = lin()
            val nameLine = lin(vertical = false)
            nameLine.addView(label(server.nickname, SP_XL, portalColor(identity), BOLD, lines = 1), lp(WRAP, WRAP))
            if (portalRow.isNew) nameLine.addView(newMark(), lp(WRAP, WRAP).margins(this@PortalActivity, l = 8))
            col.addView(nameLine)
            col.addView(mono("${portalRow.host} · @${server.userName}", SP_S, color(R.color.text_muted), lines = 1), lp().margins(this@PortalActivity, t = 3))
            addView(col, lp(0, WRAP, 1f))
        }
        body.addView(head, lp().margins(this, t = 8))
        val where = lin(vertical = false).apply { setPadding(dp(4), dp(8), dp(4), 0) }
        where.addView(label(code, SP_S, fg, MEDIUM, lines = 1))
        portalRow.lastSyncMs?.let { where.addView(label("  ·  " + getString(R.string.portal_synced, clockTime(it, now)), SP_S, color(R.color.text_muted), lines = 1)) }
        body.addView(where)

        KeyChangedModel.of(server)?.let { renderKeyChanged(it, server) }
        renderIdentity(server, portalRow)

        renderSettingsRow()
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

        renderActions(server)

        body.addView(button(getString(R.string.server_remove), ButtonKind.DANGER) { confirmRemove(server) },
            lp().margins(this, t = 16, b = 20))
    }

    /** The key-changed block (mock m9): the old fingerprint, what to do, and the two choices. A revoked device gets a shorter block. */
    private fun renderKeyChanged(m: KeyChangedModel, server: Server) {
        val fg = color(R.color.danger)
        val box = lin(padH = 12, padV = 12).apply { background = rounded(color(R.color.danger_soft), dpf(CARD_RADIUS), fg and 0x4DFFFFFF, dp(1)) }
        val top = lin(vertical = false).apply { gravity = android.view.Gravity.TOP }
        top.addView(icon(R.drawable.ic_shield, fg, 18), lp(dp(18), dp(18)).margins(this, r = 10, t = 1))
        val texts = lin()
        texts.addView(label(getString(if (m.keyProblem) R.string.keychanged_title else R.string.revoked_title), SP_M, color(R.color.text_strong), MEDIUM))
        texts.addView(label(getString(if (m.keyProblem) R.string.keychanged_body else R.string.revoked_body), SP_S, color(R.color.text)), lp().margins(this, t = 3))
        top.addView(texts, lp(0, WRAP, 1f))
        box.addView(top)
        body.addView(box, lp().margins(this, t = 12))

        if (m.keyProblem) {
            val card = panel(padH = 14, padV = 12)
            card.addView(label(getString(R.string.keychanged_fingerprints).uppercase(), SP_XS, color(R.color.text_muted), MEDIUM))
            card.addView(label(getString(R.string.keychanged_before), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
            card.addView(mono(FingerprintText.lines(m.oldFingerprint), SP_L, color(R.color.text), lines = 2).apply {
                contentDescription = FingerprintText.spoken(m.oldFingerprint)
            }, lp().margins(this, t = 2))
            card.addView(label(getString(R.string.keychanged_new), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
            card.addView(label(getString(R.string.keychanged_new_unknown), SP_S, fg), lp().margins(this, t = 2))
            body.addView(card, lp().margins(this, t = 10))
            val ask = android.text.SpannableStringBuilder(getString(R.string.keychanged_ask_lead)).also {
                it.setSpan(android.text.style.StyleSpan(android.graphics.Typeface.BOLD), 0, it.length, 0)
                it.append(" ").append(getString(R.string.keychanged_ask_body))
            }
            body.addView(label(ask, SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), dp(10), dp(4), 0) })
            body.addView(label(getString(R.string.keychanged_continue), SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), dp(8), dp(4), 0) })
        }
        body.addView(button(getString(R.string.keychanged_enter_code)) { reenroll(server.id) }, lp().margins(this, t = 12))
        body.addView(button(getString(R.string.keychanged_remove), ButtonKind.DANGER) { confirmRemove(server) }, lp().margins(this, t = 8))
    }

    /** What the phone knows about the identity of this portal: name, color, full fingerprint, what the portal calls itself. */
    private fun renderIdentity(server: Server, info: PortalRow) {
        body.addView(sectionTitle(getString(R.string.portal_identity)))
        body.addView(label(getString(R.string.portal_identity_help), SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), 0, dp(4), dp(8)) })
        val p = panel()
        p.addView(row(getString(R.string.portal_name), server.nickname, icon(R.drawable.ic_chevron_right, color(R.color.text_muted), 18)) { renameSheet(server) }
            .apply { minimumHeight = dp(48) })
        p.addView(divider())
        val swatch = View(this).apply { background = rounded(portalColor(server.colorIndex), dpf(5f)) }
        val colorName = PortalPalette.name(server.colorIndex).replaceFirstChar { it.uppercase() }
        p.addView(row(getString(R.string.portal_color), colorName, swatch.also { it.layoutParams = LinearLayout.LayoutParams(dp(22), dp(22)) }) { colorSheet(server) }
            .apply { minimumHeight = dp(48) })
        p.addView(divider())
        val fp = lin(padH = 14, padV = 10)
        fp.addView(label(getString(R.string.portal_fingerprint), SP_S, color(R.color.text_muted)))
        fp.addView(mono(FingerprintText.lines(info.identity.fingerprint), SP_L, color(R.color.text_strong), bold = true, lines = 2).apply {
            contentDescription = FingerprintText.spoken(info.identity.fingerprint)
        }, lp().margins(this, t = 4))
        fp.addView(label(getString(R.string.portal_fingerprint_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4))
        p.addView(fp)
        server.prevKeyFp?.let { prev ->
            p.addView(divider())
            val old = lin(padH = 14, padV = 10)
            old.addView(label(getString(R.string.portal_prev_key), SP_S, color(R.color.text_muted)))
            old.addView(mono(app.portalinbox.portal.Fingerprint.display(prev), SP_S, color(R.color.text_muted), lines = 2), lp().margins(this, t = 2))
            server.keyChangedMs?.let { old.addView(label(getString(R.string.portal_prev_key_at, formatTime(it)), SP_S, color(R.color.text_muted)), lp().margins(this, t = 2)) }
            p.addView(old)
        }
        info.reportedName?.let { name ->
            p.addView(divider())
            p.addView(label(getString(R.string.portal_reported, name), SP_S, color(R.color.text_muted)).apply { setPadding(dp(14), dp(10), dp(14), dp(10)) })
        }
        body.addView(p)
    }

    /** Change address and enroll again. */
    private fun renderActions(server: Server) {
        body.addView(sectionTitle(getString(R.string.portal_connection)))
        val p = panel()
        p.addView(row(getString(R.string.portal_change_address), getString(R.string.portal_change_address_help),
            icon(R.drawable.ic_chevron_right, color(R.color.text_muted), 18)) { addressSheet(server) }.apply { minimumHeight = dp(48) })
        p.addView(divider())
        p.addView(row(getString(R.string.portal_reenroll), getString(R.string.portal_reenroll_help),
            icon(R.drawable.ic_chevron_right, color(R.color.text_muted), 18)) { reenroll(server.id) }.apply { minimumHeight = dp(48) })
        body.addView(p, lp().margins(this, t = 0))
    }

    // ------------------------------------------------------------ sheets

    private fun renameSheet(server: Server) {
        bottomSheet { sheet, dialog ->
            sheet.addView(label(getString(R.string.portal_rename_title), SP_L, color(R.color.text_strong), BOLD))
            val field = textField(server.nickname, "").apply {
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
                setSelection(text.length)
            }
            sheet.addView(field, lp().margins(this, t = 12))
            val error = label("", SP_S, color(R.color.danger)).apply { accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE }
            sheet.addView(error, lp().margins(this, t = 4))
            val save = button(getString(R.string.portal_save)) {}
            fun check(): PortalNaming.Check = PortalNaming.validate(field.text.toString(), Store.get(this).portals(), ignoreId = server.id)
            fun refresh() {
                val c = check()
                error.text = if (c is PortalNaming.Check.Invalid) when (c.reason) {
                    PortalNaming.Reason.EMPTY -> getString(R.string.name_err_empty)
                    PortalNaming.Reason.TOO_LONG -> getString(R.string.name_err_long)
                    PortalNaming.Reason.HIDDEN_CHARACTERS -> getString(R.string.name_err_hidden)
                    PortalNaming.Reason.ONLY_SEPARATORS -> getString(R.string.name_err_separators)
                    PortalNaming.Reason.TAKEN -> getString(R.string.name_err_taken, c.conflict?.nickname ?: "")
                } else ""
                save.isEnabled = c is PortalNaming.Check.Valid
                save.alpha = if (save.isEnabled) 1f else 0.4f
            }
            field.addTextChangedListener(object : android.text.TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
                override fun afterTextChanged(s: android.text.Editable?) { refresh() }
            })
            save.setOnClickListener {
                if (Store.get(this).rename(server.id, field.text.toString()) is PortalNaming.Check.Valid) {
                    dialog.dismiss()
                    render()
                } else refresh()
            }
            refresh()
            val actions = lin(vertical = false)
            actions.addView(button(getString(R.string.cancel), ButtonKind.SECONDARY) { dialog.dismiss() }, lp(0, WRAP, 1f))
            actions.addView(save, lp(0, WRAP, 1f).margins(this, l = 10))
            sheet.addView(actions, lp().margins(this, t = 12, b = 10))
        }
    }

    private fun colorSheet(server: Server) {
        bottomSheet { sheet, dialog ->
            sheet.addView(label(getString(R.string.portal_color_title, server.nickname), SP_L, color(R.color.text_strong), BOLD))
            val others = Store.get(this).portals().filter { it.id != server.id }
            val choices = (0 until PortalPalette.SIZE).map { i -> app.portalinbox.portal.EnrollPlan.ColorChoice(i, others.firstOrNull { it.colorIndex == i }) }
            sheet.addView(colorPicker(choices, server.colorIndex) { index ->
                if (Store.get(this).setColor(server.id, index)) {
                    dialog.dismiss()
                    render()
                }
            }, lp().margins(this, t = 14, b = 14))
        }
    }

    private fun addressSheet(server: Server) {
        bottomSheet { sheet, dialog ->
            sheet.addView(label(getString(R.string.portal_change_address), SP_L, color(R.color.text_strong), BOLD))
            sheet.addView(label(getString(R.string.portal_change_address_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4))
            val field = textField(server.address.removePrefix("http://").removePrefix("https://"), getString(R.string.portal_address_hint), mono = true).apply {
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or InputType.TYPE_TEXT_VARIATION_URI
                setSelection(text.length)
            }
            sheet.addView(field, lp().margins(this, t = 12))
            val error = label("", SP_S, color(R.color.danger)).apply { accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE }
            sheet.addView(error, lp().margins(this, t = 4))
            val actions = lin(vertical = false)
            actions.addView(button(getString(R.string.cancel), ButtonKind.SECONDARY) { dialog.dismiss() }, lp(0, WRAP, 1f))
            actions.addView(button(getString(R.string.portal_save)) {
                val store = Store.get(this)
                val known = store.knownPortals()
                val me = known.firstOrNull { it.identity.id == server.id } ?: return@button
                when (val r = AddressChange.check(field.text.toString(), me, known)) {
                    is AddressChange.Result.Ok -> {
                        store.setAddress(server.id, r.http)
                        // The key is the same. A stop because of the old address ends, and the connection starts again at the new one.
                        store.setReenroll(server.id, null)
                        restartConnection(server.id)
                        dialog.dismiss()
                        render()
                        snackbar(getString(R.string.portal_address_saved))
                    }
                    AddressChange.Result.Invalid -> error.text = getString(R.string.portal_address_invalid)
                    AddressChange.Result.Same -> error.text = getString(R.string.portal_address_same)
                    is AddressChange.Result.UsedBy -> error.text = getString(R.string.portal_address_used, r.portal.identity.nickname)
                }
            }, lp(0, WRAP, 1f).margins(this, l = 10))
            sheet.addView(actions, lp().margins(this, t = 12, b = 10))
        }
    }

    /** Ends the session of this portal and lets the service start a new one: it reads the address and the key again from the database. */
    private fun restartConnection(id: Long) {
        Registry.connections.remove(id)?.stop()
        InboxService.start(this)
    }

    private fun reenroll(id: Long) {
        startActivity(Intent(this, ReenrollActivity::class.java).putExtra(ReenrollActivity.EXTRA_PORTAL, id))
    }

    /** The row that opens the settings of this portal: mute, lock screen, quiet hours, summary, retention (lot MP5). */
    private fun renderSettingsRow() {
        val p = panel()
        p.addView(
            row(getString(R.string.portal_settings), getString(R.string.portal_settings_help), icon(R.drawable.ic_chevron_right, color(R.color.text_muted), 18)) {
                startActivity(PortalPrefsActivity.intent(this, serverId))
            }.apply { minimumHeight = dp(48) },
        )
        // The agents of this portal (lot G4): where the owner finds a blocked agent to unblock it.
        p.addView(divider())
        p.addView(
            row(getString(R.string.agents_title), getString(R.string.agents_row_help), icon(R.drawable.ic_chevron_right, color(R.color.text_muted), 18)) {
                startActivity(AgentActivity.list(this, serverId))
            }.apply { minimumHeight = dp(48) },
        )
        body.addView(p, lp().margins(this, t = 16))
    }

    /** One compact row per group: name and description, then the level for this phone. */
    private fun renderGroups(store: Store) {
        val levels = store.levels(serverId) // local levels; the server only knows the group ids
        val real = store.groups(serverId)
        body.addView(sectionTitle(getString(R.string.server_groups), if (real.isEmpty()) null else
            getString(R.string.server_groups_subscribed, real.count { Levels.shown(it.id, levels) != null }, real.size)))
        if (real.isEmpty()) {
            body.addView(label(getString(R.string.server_groups_none), SP_M, color(R.color.text_muted)).apply {
                setPadding(dp(4), 0, dp(4), dp(4))
            })
        }
        body.addView(label(getString(R.string.server_groups_hint), SP_S, color(R.color.text_muted)).apply {
            setPadding(dp(4), 0, dp(4), dp(10))
        })
        // The pseudo group of the alerts to the account is always listed, so the user can set it.
        val groups = Levels.withDirect(real, getString(R.string.server_direct_desc))
        val labels = listOf(getString(R.string.seg_off), getString(R.string.seg_notify), getString(R.string.seg_ring), getString(R.string.seg_alarm))
        val names = listOf(getString(R.string.action_off), getString(R.string.action_notify), getString(R.string.action_ring), getString(R.string.action_alarm))
        // Side by side when there is room, else the level control goes under the name.
        val inline = resources.configuration.screenWidthDp >= 380 && resources.configuration.fontScale < 1.3f
        val list = panel()
        groups.forEachIndexed { i, g ->
            if (i > 0) list.addView(divider())
            val current = ACTIONS.indexOf(Levels.shown(g.id, levels)).coerceAtLeast(0)
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

    /** Says what is lost, and asks once. The counts come from the database before the delete. */
    private fun confirmRemove(server: Server) {
        val summary = RemovalSummary.of(app.portalinbox.service.Items.repository(this), server.id, Store.get(this).serverAlerts(server.id).size)
        val parts = buildList {
            if (summary.openItems > 0) add(resources.getQuantityString(R.plurals.portal_remove_open, summary.openItems, summary.openItems))
            if (summary.unsentAnswers > 0) add(resources.getQuantityString(R.plurals.portal_remove_unsent, summary.unsentAnswers, summary.unsentAnswers))
            if (summary.historyItems > 0) add(resources.getQuantityString(R.plurals.portal_remove_history, summary.historyItems, summary.historyItems))
            if (summary.alerts > 0) add(resources.getQuantityString(R.plurals.portal_remove_alerts, summary.alerts, summary.alerts))
        }
        bottomSheet { sheet, dialog ->
            sheet.addView(chip(getString(R.string.server_remove_confirm), color(R.color.danger), color(R.color.danger_soft), dot = true),
                lp(WRAP, WRAP))
            sheet.addView(label(getString(R.string.server_remove_title, server.nickname), SP_L, color(R.color.text_strong), BOLD),
                lp().margins(this, t = 12))
            val lost = if (parts.isEmpty()) getString(R.string.portal_remove_nothing) else getString(R.string.portal_remove_lost, parts.joinToString(", "))
            sheet.addView(label(lost, SP_M, color(R.color.text_strong), MEDIUM), lp().margins(this, t = 8))
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
