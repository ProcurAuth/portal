package app.portalinbox.ui

import android.app.TimePickerDialog
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.view.Gravity
import android.widget.CheckBox
import android.widget.FrameLayout
import android.widget.LinearLayout
import app.portalinbox.R
import app.portalinbox.data.PrefsSync
import app.portalinbox.data.Store
import app.portalinbox.proto.DigestMode
import app.portalinbox.service.Items
import app.portalinbox.service.PendingPrefsStore
import app.portalinbox.service.Registry
import java.time.ZoneId

/**
 * The preferences of one server: the quiet hours and the summary of waiting requests (DIAL 4.9 c, CONF 7.4 c). It only draws.
 * [PrefsForm] decides what a form value means, and [PrefsSync] keeps an edit as pending until the server shows it in a `welcome`.
 *
 * A change is saved on the phone at once and sent as `set_prefs`. The server has no answer for it, so the screen says when
 * the server did not confirm the values yet. Another phone or the web can change the same values: the next `welcome` brings
 * them here.
 */
class PrefsActivity : BaseActivity() {
    companion object {
        private const val EXTRA_SERVER = "server_id"

        fun intent(context: Context, serverId: Long): Intent =
            Intent(context, PrefsActivity::class.java).putExtra(EXTRA_SERVER, serverId)
    }

    private var serverId = -1L
    private lateinit var body: LinearLayout
    private lateinit var bar: FrameLayout
    private val onChange: () -> Unit = { render() }
    private val phoneZone: String get() = ZoneId.systemDefault().id

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        if (Store.get(this).server(serverId) == null) return finish()
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

    private fun draft(): PrefsForm.Draft = PrefsForm.draftOf(PrefsSync.effective(stored()?.prefs, pending()), phoneZone)

    /** Saves a change on the phone and sends it. A value that the form refuses is not saved. */
    private fun commit(d: PrefsForm.Draft) {
        when (val r = PrefsForm.toPrefs(d)) {
            is PrefsForm.Result.Refused -> snackbar(r.reason)
            is PrefsForm.Result.Ok -> {
                val frame = PrefsSync.edit(PendingPrefsStore.get(this), serverId, r.prefs)
                Registry.connections[serverId]?.sendPrefs(frame)
            }
        }
        render()
    }

    // ------------------------------------------------------------ screen

    private fun render() {
        val server = Store.get(this).server(serverId) ?: return finish()
        body.removeAllViews()
        body.addView(sectionTitle(getString(R.string.prefs_title)))
        body.addView(label(server.serverName, SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), 0, dp(4), dp(6)) })

        val s = stored()
        if (s == null || s.bodyPurgeAfterMs <= 0) {
            body.addView(label(getString(R.string.prefs_not_enabled), SP_M, color(R.color.text_muted)).apply { setPadding(dp(4), dp(8), dp(4), dp(8)) })
            return
        }
        val d = draft()
        val pending = pending() != null
        body.addView(
            label(
                getString(if (pending) R.string.prefs_pending else R.string.prefs_confirmed) +
                    if (pending && Registry.connections[serverId]?.isConnected() != true) " " + getString(R.string.prefs_offline_note) else "",
                SP_S, color(if (pending) R.color.warning else R.color.text_muted),
            ).apply { setPadding(dp(4), 0, dp(4), dp(4)) },
        )
        renderQuiet(d)
        renderDigest(d)
        body.addView(android.view.View(this), lp(MATCH, dp(20)))
    }

    private fun renderQuiet(d: PrefsForm.Draft) {
        body.addView(sectionTitle(getString(R.string.prefs_quiet)))
        body.addView(label(getString(R.string.prefs_quiet_help), SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), 0, dp(4), dp(8)) })
        val p = panel()
        p.addView(checkRow(getString(R.string.prefs_quiet_on), d.quietOn) { on -> commit(d.copy(quietOn = on)) })
        if (d.quietOn) {
            p.addView(divider())
            p.addView(timeRow(getString(R.string.prefs_from), d.quietFrom) { commit(d.copy(quietFrom = it)) })
            p.addView(divider())
            p.addView(timeRow(getString(R.string.prefs_to), d.quietTo) { commit(d.copy(quietTo = it)) })
            p.addView(divider())
            p.addView(zoneRow(d.quietTz) { commit(d.copy(quietTz = it)) })
            p.addView(divider())
            p.addView(checkRow(getString(R.string.prefs_high_rings), d.highRings) { on -> commit(d.copy(highRings = on)) })
        }
        body.addView(p)
    }

    private fun renderDigest(d: PrefsForm.Draft) {
        body.addView(sectionTitle(getString(R.string.prefs_digest)))
        body.addView(label(getString(R.string.prefs_digest_help), SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), 0, dp(4), dp(8)) })
        val modes = listOf(DigestMode.DAILY, DigestMode.EVERY, DigestMode.OFF)
        val names = listOf(getString(R.string.prefs_digest_daily), getString(R.string.prefs_digest_every), getString(R.string.prefs_digest_off))
        val box = lin(padH = 14, padV = 10).apply { background = solidCard() }
        // Until the owner chooses, the account has no digest value and the default of the server file applies.
        val current = if (d.digestUnset) -1 else modes.indexOf(d.digestMode)
        box.addView(segmented(names, current, describe = { i -> getString(R.string.prefs_digest) + ": " + names[i] }) { i ->
            commit(d.copy(digestMode = modes[i], digestUnset = false))
        })
        if (d.digestUnset) {
            box.addView(label(getString(R.string.prefs_digest_default), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
        }
        body.addView(box)
        if (!d.digestUnset && d.digestMode == DigestMode.DAILY) {
            val p = panel()
            p.addView(timeRow(getString(R.string.prefs_at), d.digestAt) { commit(d.copy(digestAt = it, digestUnset = false)) })
            p.addView(divider())
            p.addView(zoneRow(d.digestTz) { commit(d.copy(digestTz = it, digestUnset = false)) })
            body.addView(p, lp().margins(this, t = 8))
        }
        if (!d.digestUnset && d.digestMode == DigestMode.EVERY) {
            val p = panel()
            val stepper = lin(vertical = false).apply {
                background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
                addView(iconButton(R.drawable.ic_minus, getString(R.string.prefs_every_less), color(R.color.text), 16) {
                    if (d.everyH > PrefsForm.EVERY_H_RANGE.first) commit(d.copy(everyH = d.everyH - 1, digestUnset = false))
                }.apply {
                    layoutParams = LinearLayout.LayoutParams(dp(48), dp(48))
                    alpha = if (d.everyH > PrefsForm.EVERY_H_RANGE.first) 1f else 0.35f
                })
                addView(mono(d.everyH.toString(), SP_M, color(R.color.text_strong), bold = true).apply {
                    gravity = Gravity.CENTER
                    minWidth = dp(36)
                })
                addView(iconButton(R.drawable.ic_plus, getString(R.string.prefs_every_more), color(R.color.text), 16) {
                    if (d.everyH < PrefsForm.EVERY_H_RANGE.last) commit(d.copy(everyH = d.everyH + 1, digestUnset = false))
                }.apply {
                    layoutParams = LinearLayout.LayoutParams(dp(48), dp(48))
                    alpha = if (d.everyH < PrefsForm.EVERY_H_RANGE.last) 1f else 0.35f
                })
            }
            p.addView(row(getString(R.string.prefs_every), null, stepper))
            body.addView(p, lp().margins(this, t = 8))
        }
    }

    // ------------------------------------------------------------ rows

    private fun checkRow(text: String, checked: Boolean, onToggle: (Boolean) -> Unit): android.view.View =
        CheckBox(this).apply {
            this.text = text
            textSize = SP_M
            minHeight = dp(48)
            isChecked = checked
            setTextColor(color(R.color.text))
            buttonTintList = android.content.res.ColorStateList.valueOf(color(R.color.primary))
            setPadding(dp(10), 0, dp(14), 0)
            setOnCheckedChangeListener { _, on -> if (on != checked) onToggle(on) }
        }

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
}
