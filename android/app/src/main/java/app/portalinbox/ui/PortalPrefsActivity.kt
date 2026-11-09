package app.portalinbox.ui

import android.app.TimePickerDialog
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.view.Gravity
import android.widget.FrameLayout
import android.widget.LinearLayout
import app.portalinbox.R
import app.portalinbox.alert.AttentionNotifier
import app.portalinbox.data.LocalRetention
import app.portalinbox.data.PrefsSync
import app.portalinbox.data.Store
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PrefsApply
import app.portalinbox.proto.DigestMode
import app.portalinbox.service.Items
import app.portalinbox.service.PendingPrefsStore
import app.portalinbox.service.Registry
import java.time.ZoneId

/**
 * The settings of one portal (lot MP5, proposal 3.10). It only draws: [PrefsForm] decides what a value means, [PrefsSync] keeps an
 * edit as pending until the portal shows it in a `welcome`, [PrefsApply] plans "apply to all portals", [LocalRetention] gives the
 * days to keep.
 *
 * Three kinds of values:
 * - Local to this phone and this portal, saved at once: mute, the name on the lock screen, how long to keep finished items.
 * - The preferences of the account at the portal (quiet hours, summary): saved on the phone, sent as `set_prefs`, kept as pending
 *   for a portal that is not connected. The screen shows the values that apply, and says when the portal did not confirm them or
 *   refused them.
 * - "Apply to all portals": a sheet that the user opens and confirms. Nothing is sent to another portal by itself.
 */
class PortalPrefsActivity : BaseActivity() {
    companion object {
        private const val EXTRA_SERVER = "server_id"

        fun intent(context: Context, serverId: Long): Intent =
            Intent(context, PortalPrefsActivity::class.java).putExtra(EXTRA_SERVER, serverId)
    }

    private var serverId = -1L
    private lateinit var body: LinearLayout
    private lateinit var bar: FrameLayout
    private val onChange: () -> Unit = { render() }
    private val phoneZone: String get() = ZoneId.systemDefault().id

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val server = Store.get(this).server(serverId) ?: return finish()
        title = server.nickname
        val root = lin()
        bar = FrameLayout(this)
        root.addView(bar, lp())
        bar.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }))
        body = lin(padH = 10, padV = 4)
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

    // ------------------------------------------------------------ the values

    private fun stored() = Items.repository(this).prefs(serverId)

    private fun pending() = PendingPrefsStore.get(this).get(serverId)

    /** The portal takes preferences: its last `welcome` had a purge age. A portal without requests has none. */
    private fun itemsEnabled(): Boolean = stored()?.let { it.bodyPurgeAfterMs > 0 } == true

    private fun draft(): PrefsForm.Draft = PrefsForm.draftOf(PrefsSync.effective(stored()?.prefs, pending()), phoneZone)

    /**
     * Saves an edit of the account on the phone and sends it. A value that the form refuses is not saved: the reason comes back, and
     * nothing is changed to make it fit. Returns null when it was saved.
     */
    private fun commit(d: PrefsForm.Draft): String? = when (val r = PrefsForm.toPrefs(d)) {
        is PrefsForm.Result.Refused -> r.reason
        is PrefsForm.Result.Ok -> {
            Registry.setRefusal(serverId, null)
            val frame = PrefsSync.edit(PendingPrefsStore.get(this), serverId, r.prefs)
            Registry.connections[serverId]?.sendPrefs(frame)
            render()
            null
        }
    }

    /** An edit from a switch of the screen: a refusal is told in a snackbar. */
    private fun commitFromScreen(d: PrefsForm.Draft) {
        commit(d)?.let { snackbar(it) }
    }

    // ------------------------------------------------------------ screen

    private fun render() {
        val store = Store.get(this)
        val server = store.server(serverId) ?: return finish()
        val identity = PortalIdentity.of(server)
        body.removeAllViews()
        body.addView(header(identity), lp().margins(this, t = 8))

        // Local switches.
        body.addView(sectionTitle(getString(R.string.portal_prefs_notifications)))
        val notify = panel()
        val muted = server.mutedUntilMs?.let { it > System.currentTimeMillis() } == true
        notify.addView(switchRow(getString(R.string.portal_prefs_mute), getString(R.string.portal_prefs_mute_help), muted, { on ->
            store.setMuted(serverId, if (on) Long.MAX_VALUE else null)
            AttentionNotifier.refresh(this)
        }))
        notify.addView(divider())
        notify.addView(switchRow(getString(R.string.portal_prefs_lock_name), getString(R.string.portal_prefs_lock_name_help), !server.hideNameOnLock, { show ->
            store.setHideNameOnLock(serverId, !show)
            AttentionNotifier.refresh(this)
        }))
        body.addView(notify)

        // The account at the portal.
        if (!itemsEnabled()) {
            body.addView(label(getString(R.string.prefs_not_enabled), SP_M, color(R.color.text_muted)).apply { setPadding(dp(4), dp(18), dp(4), dp(8)) })
        } else {
            val d = draft()
            renderStatus()
            renderQuiet(d)
            renderSummary(d)
        }

        // This phone.
        body.addView(sectionTitle(getString(R.string.portal_prefs_on_phone)))
        val phone = panel()
        val global = Prefs.localRetentionDays(this)
        phone.addView(
            row(getString(R.string.portal_prefs_keep), getString(R.string.portal_prefs_keep_help),
                label(LocalRetention.overrideLabel(server.retentionDays, global), SP_S, color(R.color.text_muted))) { retentionSheet(global) }
                .apply { minimumHeight = dp(56) },
        )
        body.addView(phone)

        if (itemsEnabled()) {
            body.addView(button(getString(R.string.portal_prefs_apply_all), ButtonKind.SECONDARY) { applySheet() }, lp().margins(this, t = 12, b = 20))
        } else {
            body.addView(android.view.View(this), lp(MATCH, dp(20)))
        }
    }

    /** The portal that these settings belong to: monogram, name in its color, and the words that say what they cover. */
    private fun header(identity: PortalIdentity): LinearLayout = lin(vertical = false, padH = 12, padV = 10).apply {
        val c = portalColor(identity)
        background = rounded(c and 0x00FFFFFF or 0x1F000000, dpf(CARD_RADIUS), c and 0x00FFFFFF or 0x66000000, dp(1))
        gravity = Gravity.CENTER_VERTICAL
        addView(monogramSquare(identity, 40), lp(dp(40), dp(40)).margins(this@PortalPrefsActivity, r = 12))
        val col = lin()
        col.addView(label(identity.nickname, SP_XL, c, BOLD, lines = 1))
        col.addView(label(getString(R.string.portal_prefs_only), SP_S, color(R.color.text_muted)), lp().margins(this@PortalPrefsActivity, t = 3))
        addView(col, lp(0, WRAP, 1f))
    }

    /** Says when the portal did not confirm the values, or refused them. Nothing when the portal has them. */
    private fun renderStatus() {
        val refusal = Registry.refusal(serverId)
        val connected = Registry.connections[serverId]?.isConnected() == true
        val (text, c) = when (PrefsForm.status(pending() != null, connected, refusal != null)) {
            PrefsForm.Status.CONFIRMED -> return
            PrefsForm.Status.WAITING -> getString(R.string.prefs_pending) to R.color.warning
            PrefsForm.Status.WAITING_OFFLINE -> (getString(R.string.prefs_pending) + " " + getString(R.string.prefs_offline_note)) to R.color.warning
            PrefsForm.Status.REFUSED -> getString(R.string.portal_prefs_refused, PrefsForm.refusalLine(refusal.orEmpty())) to R.color.danger
        }
        body.addView(
            label(text, SP_S, color(c)).apply {
                setPadding(dp(4), dp(14), dp(4), 0)
                accessibilityLiveRegion = android.view.View.ACCESSIBILITY_LIVE_REGION_POLITE
            },
        )
    }

    private fun renderQuiet(d: PrefsForm.Draft) {
        body.addView(sectionTitle(getString(R.string.prefs_quiet), getString(R.string.portal_prefs_sent)))
        val p = panel()
        val q = PrefsForm.toPrefs(d).let { (it as? PrefsForm.Result.Ok)?.prefs?.quiet }
        if (d.quietOn && q != null) {
            p.addView(
                switchRow(PrefsForm.quietTitle(q), PrefsForm.quietDetail(q), true, { on -> commitFromScreen(d.copy(quietOn = on)) },
                    onOpen = { quietSheet() }, switchLabel = getString(R.string.prefs_quiet)),
            )
            p.addView(divider())
            p.addView(switchRow(getString(R.string.portal_prefs_urgent_rings), null, d.highRings, { on -> commitFromScreen(d.copy(highRings = on)) }))
        } else {
            p.addView(
                switchRow(getString(R.string.portal_prefs_quiet_off), getString(R.string.portal_prefs_quiet_off_help), false, { on -> commitFromScreen(d.copy(quietOn = on)) }),
            )
        }
        body.addView(p)
    }

    private fun renderSummary(d: PrefsForm.Draft) {
        body.addView(sectionTitle(getString(R.string.portal_prefs_summary), getString(R.string.portal_prefs_sent)))
        val digest = (PrefsForm.toPrefs(d) as? PrefsForm.Result.Ok)?.prefs?.digest
        val p = panel()
        p.addView(
            row(
                PrefsForm.digestTitle(digest),
                PrefsForm.digestDetail(digest) ?: if (digest == null) getString(R.string.portal_prefs_default_help) else null,
                label(getString(R.string.portal_prefs_change), SP_S, color(R.color.text_muted)),
            ) { summarySheet() }.apply { minimumHeight = dp(56) },
        )
        body.addView(p)
    }

    // ------------------------------------------------------------ sheets: quiet hours and summary

    private fun quietSheet() {
        var d = draft()
        bottomSheet { sheet, dialog ->
            val content = lin()
            sheet.addView(content)
            fun draw() {
                content.removeAllViews()
                content.addView(label(getString(R.string.portal_prefs_quiet_title, Store.get(this).server(serverId)?.nickname.orEmpty()), SP_L, color(R.color.text_strong), BOLD))
                val p = panel()
                p.addView(timeRow(getString(R.string.prefs_from), d.quietFrom) { d = d.copy(quietFrom = it); draw() })
                p.addView(divider())
                p.addView(timeRow(getString(R.string.prefs_to), d.quietTo) { d = d.copy(quietTo = it); draw() })
                p.addView(divider())
                p.addView(zoneRow(d.quietTz) { d = d.copy(quietTz = it); draw() })
                content.addView(p, lp().margins(this, t = 12))
                saveBar(content, dialog) { commit(d.copy(quietOn = true)) }
            }
            draw()
        }
    }

    private fun summarySheet() {
        var d = draft()
        bottomSheet { sheet, dialog ->
            val content = lin()
            sheet.addView(content)
            fun draw() {
                content.removeAllViews()
                content.addView(label(getString(R.string.portal_prefs_summary_title, Store.get(this).server(serverId)?.nickname.orEmpty()), SP_L, color(R.color.text_strong), BOLD))
                content.addView(label(getString(R.string.prefs_digest_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4, b = 10))
                val modes = listOf(DigestMode.DAILY, DigestMode.EVERY, DigestMode.OFF)
                val names = listOf(getString(R.string.prefs_digest_daily), getString(R.string.prefs_digest_every), getString(R.string.prefs_digest_off))
                // Until the owner chooses, the account has no summary value and the default of the portal applies.
                val current = if (d.digestUnset) -1 else modes.indexOf(d.digestMode)
                content.addView(segmented(names, current, describe = { i -> getString(R.string.prefs_digest) + ": " + names[i] }) { i ->
                    d = d.copy(digestMode = modes[i], digestUnset = false)
                    draw()
                })
                if (d.digestUnset) {
                    content.addView(label(getString(R.string.prefs_digest_default), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
                }
                if (!d.digestUnset && d.digestMode == DigestMode.DAILY) {
                    val p = panel()
                    p.addView(timeRow(getString(R.string.prefs_at), d.digestAt) { d = d.copy(digestAt = it); draw() })
                    p.addView(divider())
                    p.addView(zoneRow(d.digestTz) { d = d.copy(digestTz = it); draw() })
                    content.addView(p, lp().margins(this, t = 8))
                }
                if (!d.digestUnset && d.digestMode == DigestMode.EVERY) {
                    val p = panel()
                    p.addView(row(getString(R.string.prefs_every), null, stepper(d.everyH) { d = d.copy(everyH = it); draw() }))
                    content.addView(p, lp().margins(this, t = 8))
                }
                saveBar(content, dialog) { commit(d) }
            }
            draw()
        }
    }

    /** The reason that a sheet cannot save, then Cancel and Save. Save closes the sheet only when the values were accepted. */
    private fun saveBar(content: LinearLayout, dialog: android.app.Dialog, save: () -> String?) {
        val error = label("", SP_S, color(R.color.danger)).apply { accessibilityLiveRegion = android.view.View.ACCESSIBILITY_LIVE_REGION_POLITE }
        content.addView(error, lp().margins(this, t = 8))
        val actions = lin(vertical = false)
        actions.addView(button(getString(R.string.cancel), ButtonKind.SECONDARY) { dialog.dismiss() }, lp(0, WRAP, 1f))
        actions.addView(button(getString(R.string.portal_save)) {
            val reason = save()
            if (reason == null) dialog.dismiss() else error.text = reason
        }, lp(0, WRAP, 1f).margins(this, l = 10))
        content.addView(actions, lp().margins(this, t = 8, b = 10))
    }

    // ------------------------------------------------------------ sheet: keep finished items

    private fun retentionSheet(global: Int) {
        val store = Store.get(this)
        val server = store.server(serverId) ?: return
        bottomSheet { sheet, dialog ->
            val content = lin()
            sheet.addView(content)
            fun choose(days: Int?) {
                store.setRetentionDays(serverId, days)
                // What the sheet said would go goes now, so the screen and the phone agree.
                LocalRetention.purge(Items.repository(this), serverId, System.currentTimeMillis(), LocalRetention.daysFor(global, days))
                dialog.dismiss()
                render()
            }
            fun list() {
                content.removeAllViews()
                content.addView(label(getString(R.string.portal_prefs_keep_title, server.nickname), SP_L, color(R.color.text_strong), BOLD))
                content.addView(label(getString(R.string.portal_prefs_keep_sheet_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4, b = 10))
                val p = panel()
                val current = store.server(serverId)?.retentionDays?.takeIf { LocalRetention.isOption(it) }
                LocalRetention.overrideChoices().forEachIndexed { i, days ->
                    if (i > 0) p.addView(divider())
                    val mark = if (days == current) icon(R.drawable.ic_check, color(R.color.primary), 18) else null
                    p.addView(row(LocalRetention.overrideLabel(days, global), null, mark) { confirmOrChoose(days, global, content, ::choose, ::list) }.apply {
                        minimumHeight = dp(48)
                        if (days == current) contentDescription = LocalRetention.overrideLabel(days, global) + ", " + getString(R.string.a11y_selected)
                    })
                }
                content.addView(p)
                content.addView(android.view.View(this), lp(MATCH, dp(10)))
            }
            list()
        }
    }

    /** A value that would erase finished items asks first. A value that erases nothing is saved at once. */
    private fun confirmOrChoose(days: Int?, global: Int, content: LinearLayout, choose: (Int?) -> Unit, back: () -> Unit) {
        val effective = LocalRetention.daysFor(global, days)
        val erase = LocalRetention.wouldErase(Items.repository(this), serverId, System.currentTimeMillis(), effective)
        if (erase == 0) return choose(days)
        content.removeAllViews()
        content.addView(label(resources.getQuantityString(R.plurals.portal_prefs_keep_erase, erase, erase), SP_M, color(R.color.text)))
        val actions = lin(vertical = false)
        actions.addView(button(getString(R.string.cancel), ButtonKind.SECONDARY) { back() }, lp(0, WRAP, 1f))
        actions.addView(button(getString(R.string.portal_prefs_keep_erase_confirm), ButtonKind.DANGER) { choose(days) }, lp(0, WRAP, 1f).margins(this, l = 10))
        content.addView(actions, lp().margins(this, t = 14, b = 10))
    }

    // ------------------------------------------------------------ sheet: apply to all portals

    /** The sheet of "Apply to all portals?": one line for each other portal, with what will happen. The user chooses and confirms. */
    private fun applySheet() {
        val store = Store.get(this)
        val me = store.server(serverId) ?: return
        val others = store.servers().filter { it.id != serverId }
        if (others.isEmpty()) return snackbar(getString(R.string.portal_prefs_apply_none))
        val repo = Items.repository(this)
        val pendingStore = PendingPrefsStore.get(this)
        val values = PrefsSync.effective(stored()?.prefs, pending())
        val targets = others.map { s ->
            val st = repo.prefs(s.id)
            val enabled = st != null && st.bodyPurgeAfterMs > 0
            PrefsApply.Target(
                identity = PortalIdentity.of(s), state = Registry.status(s.id).state, itemsEnabled = enabled, account = s.userName,
                effective = if (enabled) PrefsSync.effective(st?.prefs, pendingStore.get(s.id)) else null,
            )
        }
        val steps = PrefsApply.plan(me.userName, targets, values)
        val chosen = steps.filter { it.ticked }.map { it.target.identity.id }.toMutableSet()
        bottomSheet { sheet, dialog ->
            sheet.addView(label(getString(R.string.portal_prefs_apply_title), SP_L, color(R.color.text_strong), BOLD))
            sheet.addView(label(PrefsApply.describe(values), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4, b = 10))
            val apply = button(PrefsApply.buttonText(chosen.size)) {}
            fun refreshButton() {
                apply.text = PrefsApply.buttonText(chosen.size)
                apply.isEnabled = chosen.isNotEmpty()
                apply.alpha = if (chosen.isEmpty()) 0.4f else 1f
            }
            val list = panel()
            steps.forEachIndexed { i, step ->
                if (i > 0) list.addView(divider())
                val id = step.target.identity.id
                list.addView(applyLine(step.target.identity, step, id in chosen) { on ->
                    if (on) chosen.add(id) else chosen.remove(id)
                    refreshButton()
                })
            }
            sheet.addView(list)
            val actions = lin(vertical = false)
            actions.addView(button(getString(R.string.cancel), ButtonKind.SECONDARY) { dialog.dismiss() }, lp(0, WRAP, 1f))
            apply.setOnClickListener {
                val out = PrefsApply.run(steps, chosen, values, pendingStore) { id, frame ->
                    Registry.setRefusal(id, null)
                    Registry.connections[id]?.sendPrefs(frame)
                }
                dialog.dismiss()
                PrefsApply.resultText(out)?.let { snackbar(it) }
                render()
            }
            actions.addView(apply, lp(0, WRAP, 1f).margins(this, l = 10))
            sheet.addView(actions, lp().margins(this, t = 14, b = 10))
            refreshButton()
        }
    }

    // ------------------------------------------------------------ rows

    /** A row with a name and the time: tapping it opens the clock. A 48 dp target. */
    private fun timeRow(name: String, hhmm: String, onPick: (String) -> Unit): android.view.View =
        row(name, null, mono(hhmm, SP_L, color(R.color.text_strong), bold = true)) {
            TimePickerDialog(this, { _, h, m -> onPick(PrefsForm.hhmm(h, m)) }, PrefsForm.hour(hhmm), PrefsForm.minute(hhmm), true).show()
        }.apply {
            minimumHeight = dp(48)
            contentDescription = "$name, $hhmm"
        }

    /** The zone of a window. The owner can switch to the zone of this phone: the list of all zones does not fit a phone screen. */
    private fun zoneRow(tz: String, onPick: (String) -> Unit): android.view.View {
        val differs = tz != phoneZone
        return row(
            getString(R.string.prefs_zone), if (differs) getString(R.string.prefs_zone_phone) else null,
            mono(tz, SP_S, color(R.color.text_strong)),
            if (differs) ({ onPick(phoneZone) }) else null,
        ).apply { minimumHeight = dp(48) }
    }

    /** The number of hours of an "every" summary, with two buttons. */
    private fun stepper(hours: Int, onChange: (Int) -> Unit): LinearLayout = lin(vertical = false).apply {
        background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
        val first = PrefsForm.EVERY_H_RANGE.first
        val last = PrefsForm.EVERY_H_RANGE.last
        addView(iconButton(R.drawable.ic_minus, getString(R.string.prefs_every_less), color(R.color.text), 16) {
            if (hours > first) onChange(hours - 1)
        }.apply {
            layoutParams = LinearLayout.LayoutParams(dp(48), dp(48))
            alpha = if (hours > first) 1f else 0.35f
        })
        addView(mono(hours.toString(), SP_M, color(R.color.text_strong), bold = true).apply {
            gravity = Gravity.CENTER
            minWidth = dp(36)
        })
        addView(iconButton(R.drawable.ic_plus, getString(R.string.prefs_every_more), color(R.color.text), 16) {
            if (hours < last) onChange(hours + 1)
        }.apply {
            layoutParams = LinearLayout.LayoutParams(dp(48), dp(48))
            alpha = if (hours < last) 1f else 0.35f
        })
    }
}
