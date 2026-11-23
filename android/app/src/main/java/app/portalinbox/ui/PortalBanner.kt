package app.portalinbox.ui

import android.content.Context
import android.content.res.ColorStateList
import android.content.res.Configuration
import android.view.Gravity
import android.view.View
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.data.Server
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PortalLabels
import app.portalinbox.portal.PortalPalette

/**
 * The identity of the portal on the approval screen. Only drawing: the texts are in [PortalLabels]. The user
 * chose the nickname and the color. The portal cannot change them, so a look-alike portal cannot look like the real one.
 */
object PortalBanner {
    private fun night(context: Context) =
        (context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES

    /** The banner above the title: a rail of 6 dp, the monogram, the nickname, the host and the login, and the start of the key. */
    fun banner(context: Context, server: Server): View = with(context) {
        val id = PortalIdentity.of(server)
        val c = id.color(night(this))
        val tint = PortalPalette.over((c and 0x00FFFFFF) or (0x33 shl 24), color(R.color.card))
        val row = lin(vertical = false).apply {
            setBackgroundColor(tint)
            contentDescription = getString(R.string.approval_banner_description, id.nickname, id.shortFingerprint())
        }
        row.addView(View(this).apply { setBackgroundColor(c) }, lp(dp(6), MATCH))
        row.addView(monogram(this, id, 40), lp(dp(40), dp(40)).margins(this, l = 12, t = 10, b = 10))
        val names = lin()
        names.addView(label(VisibleText.escapeLine(id.nickname), SP_L + 2f, c, BOLD, lines = 1))
        names.addView(mono(PortalLabels.hostAndLogin(server), SP_XS, color(R.color.text_muted)), lp().margins(this, t = 2))
        row.addView(names, lp(0, WRAP, 1f).margins(this, l = 12, r = 8))
        val key = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        key.addView(ImageView(this).apply {
            setImageResource(R.drawable.ic_key_shield)
            imageTintList = ColorStateList.valueOf(color(R.color.text_muted))
        }, lp(dp(14), dp(14)).margins(this, r = 4))
        key.addView(label(PortalLabels.bannerKey(id), SP_XS, color(R.color.text_muted)))
        row.addView(key, lp().margins(this, r = 14))
        val box = lin()
        box.addView(divider().apply { setBackgroundColor(c and 0x00FFFFFF or (0x8C shl 24)) })
        box.addView(row)
        box.addView(divider().apply { setBackgroundColor(c and 0x00FFFFFF or (0x8C shl 24)) })
        box
    }

    /** The strip above the action bar: a border of 2 dp in the portal color, the nickname and the whole fingerprint. */
    fun footer(context: Context, server: Server): View = with(context) {
        val id = PortalIdentity.of(server)
        val c = id.color(night(this))
        val box = lin().apply {
            setBackgroundColor(PortalPalette.over((c and 0x00FFFFFF) or (0x1F shl 24), color(R.color.bg_page)))
            contentDescription = getString(R.string.approval_footer_description, id.nickname, id.fingerprint)
        }
        box.addView(View(this).apply { setBackgroundColor(c) }, lp(MATCH, dp(2)))
        box.addView(
            mono(PortalLabels.footer(id), SP_XS, color(R.color.text_muted)).apply { setPadding(dp(12), dp(6), dp(12), dp(6)) },
            lp(),
        )
        box
    }

    /** The square of the portal color with the first letters of the nickname. */
    fun monogram(context: Context, id: PortalIdentity, sizeDp: Int): TextView = with(context) {
        label(id.monogram, if (sizeDp >= 32) SP_L else SP_XS - 2f, id.onColor(night(this)), BOLD).apply {
            gravity = Gravity.CENTER
            background = rounded(id.color(night(this@with)), dpf(sizeDp / 4f))
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }
    }
}
