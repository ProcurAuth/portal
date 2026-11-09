package app.portalinbox.ui

import android.content.Context
import android.os.Bundle
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.LinearLayout
import app.portalinbox.R
import app.portalinbox.data.Server
import app.portalinbox.data.Store
import app.portalinbox.portal.PortalAdmission
import app.portalinbox.portal.PortalReorder
import app.portalinbox.portal.PortalRow
import app.portalinbox.service.Items
import app.portalinbox.service.LinkState
import app.portalinbox.service.Registry

/**
 * The portal list (drawer entry "Portals", mock m2): one row per portal in the order of the user, with the monogram, the nickname, the
 * host and login, the start of the key, the state, the last sync and the open count. A drag handle changes the order. The home screen
 * puts this view in its body. Tapping a row opens the screen of the portal.
 */
object PortalListView {
    /** Ids of the screen reader actions of a row. */
    private val ACTION_UP = View.generateViewId()
    private val ACTION_DOWN = View.generateViewId()

    /**
     * Adds the portal list to [body]. [onOpen] opens the screen of a portal. [onAdd] opens the enrollment. [onChanged] draws the
     * screen again after a change of the order.
     */
    fun render(context: Context, body: LinearLayout, servers: List<Server>, onOpen: (Long) -> Unit, onAdd: () -> Unit, onChanged: () -> Unit) {
        with(context) {
            val connected = servers.count { Registry.status(it.id).state == LinkState.CONNECTED }
            body.addView(sectionTitle(getString(R.string.servers_title),
                if (servers.isEmpty()) null else resources.getQuantityString(R.plurals.servers_connected, servers.size, connected, servers.size)))
            if (servers.isEmpty()) {
                body.addView(emptyBox())
            } else {
                body.addView(list(servers, onOpen, onChanged))
                body.addView(footer(), lp().margins(this, t = 10))
            }
            if (servers.size >= PortalAdmission.MAX_PORTALS) {
                body.addView(label(getString(R.string.portals_cap_note, PortalAdmission.MAX_PORTALS), SP_S, color(R.color.text_muted)).apply {
                    setPadding(dp(4), dp(10), dp(4), dp(16))
                })
            } else {
                body.addView(button(getString(R.string.servers_add), ButtonKind.SECONDARY) { onAdd() }, lp().margins(this, t = 10, b = 16))
            }
        }
    }

    private fun Context.emptyBox(): View {
        val c = panel(padH = 14, padV = 18)
        c.addView(label(getString(R.string.alerts_no_server_title), SP_L, color(R.color.text_strong), MEDIUM).apply { gravity = Gravity.CENTER })
        c.addView(label(getString(R.string.alerts_no_server_body), SP_M, color(R.color.text_muted)).apply { gravity = Gravity.CENTER }, lp().margins(this, t = 4))
        return c
    }

    private fun Context.footer(): LinearLayout {
        val box = lin()
        val lead = android.text.SpannableStringBuilder(getString(R.string.portals_footer_lead)).also {
            it.setSpan(android.text.style.ForegroundColorSpan(color(R.color.text)), 0, it.length, 0)
            it.append(" ").append(getString(R.string.portals_footer_body))
        }
        box.addView(label(lead, SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), 0, dp(4), 0) })
        val key = android.text.SpannableStringBuilder(getString(R.string.portals_key_lead)).also {
            it.setSpan(android.text.style.ForegroundColorSpan(color(R.color.text)), 0, it.length, 0)
            it.append(" ").append(getString(R.string.portals_key_body))
        }
        box.addView(label(key, SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), dp(8), dp(4), 0) })
        return box
    }

    private fun Context.list(servers: List<Server>, onOpen: (Long) -> Unit, onChanged: () -> Unit): LinearLayout {
        val now = System.currentTimeMillis()
        val repo = Items.repository(this)
        val store = Store.get(this)
        val panel = panel()
        val rows = servers.map { s ->
            val model = PortalRow.of(s, Registry.status(s.id), repo.openItems(s.id).size, now)
            portalRow(model, now, onOpen)
        }
        rows.forEachIndexed { i, r ->
            r.view.tag = servers[i].id
            panel.addView(r.view)
            val id = servers[i].id
            val name = servers[i].nickname
            r.handle.contentDescription = getString(R.string.portal_drag, name)
            r.handle.setOnTouchListener(DragListener(this, panel, r.view) { order ->
                store.reorder(order)
                onChanged()
            })
            // The same move without a drag, for a screen reader.
            r.tap.accessibilityDelegate = object : View.AccessibilityDelegate() {
                override fun onInitializeAccessibilityNodeInfo(host: View, info: AccessibilityNodeInfo) {
                    super.onInitializeAccessibilityNodeInfo(host, info)
                    info.addAction(AccessibilityNodeInfo.AccessibilityAction(ACTION_UP, getString(R.string.portal_move_up)))
                    info.addAction(AccessibilityNodeInfo.AccessibilityAction(ACTION_DOWN, getString(R.string.portal_move_down)))
                }

                override fun performAccessibilityAction(host: View, action: Int, args: Bundle?): Boolean {
                    val current = (0 until panel.childCount).map { panel.getChildAt(it).tag as Long }
                    val next = when (action) {
                        ACTION_UP -> PortalReorder.up(current, id)
                        ACTION_DOWN -> PortalReorder.down(current, id)
                        else -> return super.performAccessibilityAction(host, action, args)
                    }
                    store.reorder(next)
                    onChanged()
                    host.announceForAccessibility(getString(R.string.portal_moved, name, next.indexOf(id) + 1, next.size))
                    return true
                }
            }
        }
        return panel
    }

    private class RowViews(val view: LinearLayout, val tap: View, val handle: View)

    private fun Context.portalRow(m: PortalRow, now: Long, onOpen: (Long) -> Unit): RowViews {
        val id = m.identity.id
        val (stateText, stateColor) = stateOf(m, now)
        val outer = lin(vertical = false)
        outer.minimumHeight = dp(84)
        outer.background = rowRipple()
        outer.isClickable = true
        outer.isFocusable = true
        outer.setOnClickListener { onOpen(id) }
        // A portal that is not connected is dimmed a little: its requests may be old.
        outer.alpha = if (m.state == LinkState.CONNECTED || m.state == LinkState.CONNECTING) 1f else 0.9f
        outer.addView(View(this).apply { setBackgroundColor(portalColor(m.identity)) }, lp(dp(3), MATCH))

        val handle = icon(R.drawable.ic_drag, color(R.color.text_subtle), 20).apply {
            // A 40 dp wide target for the finger. It is not a click target: only a drag moves the row.
            setPadding(dp(8), dp(0), dp(8), dp(0))
            layoutParams = LinearLayout.LayoutParams(dp(36), MATCH)
            scaleType = android.widget.ImageView.ScaleType.CENTER
        }
        outer.addView(handle)
        outer.addView(monogramSquare(m.identity, 40), lp(dp(40), dp(40)).margins(this, r = 12))

        val col = lin().apply { setPadding(0, dp(10), 0, dp(10)) }
        val nameLine = lin(vertical = false)
        nameLine.addView(label(m.identity.nickname, SP_L, color(R.color.text_strong), MEDIUM, lines = 1), lp(WRAP, WRAP))
        if (m.isNew) nameLine.addView(newMark(), lp(WRAP, WRAP).margins(this, l = 6))
        col.addView(nameLine)
        col.addView(label("${m.host} · @${m.login}", SP_S, color(R.color.text_muted), lines = 1), lp().margins(this, t = 2))
        col.addView(mono(getString(R.string.portal_key_prefix, m.keyPrefix), SP_XS, color(R.color.text_subtle)), lp().margins(this, t = 2))
        outer.addView(col, lp(0, WRAP, 1f))

        val right = lin().apply { gravity = Gravity.END; setPadding(dp(8), dp(10), dp(12), dp(10)) }
        right.addView(label(stateText, SP_S, stateColor, MEDIUM, lines = 1).apply { gravity = Gravity.END })
        right.addView(label(m.lastSyncMs?.let { getString(R.string.portal_synced, clockTime(it, now)) } ?: getString(R.string.portal_never_synced),
            SP_XS, color(R.color.text_muted), lines = 1).apply { gravity = Gravity.END }, lp().margins(this, t = 4))
        right.addView(label(resources.getQuantityString(R.plurals.portal_open_count, m.openCount, m.openCount), SP_XS, color(R.color.text_muted), lines = 1)
            .apply { gravity = Gravity.END }, lp().margins(this, t = 2))
        outer.addView(right, lp(WRAP, WRAP))

        // The divider under each row sits inside it, so a row keeps its line when it moves.
        val wrap = lin()
        wrap.addView(outer)
        wrap.addView(divider())
        val spoken = "${m.identity.nickname}, ${m.host}, $stateText, ${resources.getQuantityString(R.plurals.portal_open_count, m.openCount, m.openCount)}"
        wrap.contentDescription = spoken
        outer.contentDescription = spoken
        wrap.tag = id
        return RowViews(wrap, outer, handle)
    }

    private fun Context.stateOf(m: PortalRow, now: Long): Pair<String, Int> = when (m.state) {
        LinkState.CONNECTED -> getString(R.string.status_online) to color(R.color.primary)
        LinkState.CONNECTING -> (m.stateDetail ?: getString(R.string.status_connecting)) to color(R.color.warning)
        LinkState.DISCONNECTED -> getString(R.string.status_offline_since, shortAge(m.stateSinceMs, now)) to color(R.color.danger)
        LinkState.REENROLL -> getString(R.string.status_reenroll) to color(R.color.danger)
        LinkState.KEY_CHANGED -> getString(R.string.status_key_changed) to color(R.color.danger)
    }

    /**
     * The drag of one row by its handle. The row follows the finger. When its middle passes the middle of a neighbour, the two
     * swap places in the list ([PortalReorder.targetIndex]). When the finger lifts, [onDone] gets the new order.
     */
    private class DragListener(
        private val context: Context,
        private val list: LinearLayout,
        private val row: View,
        private val onDone: (List<Long>) -> Unit,
    ) : View.OnTouchListener {
        private var startRawY = 0f
        private var anchorTop = 0
        private var moved = false

        override fun onTouch(handle: View, e: MotionEvent): Boolean {
            when (e.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    startRawY = e.rawY
                    anchorTop = row.top
                    moved = false
                    row.elevation = context.dpf(6f)
                    row.alpha = 0.95f
                    list.parent?.requestDisallowInterceptTouchEvent(true)
                    return true
                }
                MotionEvent.ACTION_MOVE -> {
                    val dy = e.rawY - startRawY
                    val heights = (0 until list.childCount).map { list.getChildAt(it).height }
                    // The tops come from the heights, not from the views: after a swap the views are not laid out again yet.
                    val tops = heights.runningFold(0) { sum, h -> sum + h }.dropLast(1)
                    val target = PortalReorder.targetIndex(tops, heights, anchorTop + dy + row.height / 2f)
                    if (target != list.indexOfChild(row)) {
                        list.removeView(row)
                        list.addView(row, target)
                        moved = true
                    }
                    // The row stays under the finger: its place in the list plus a shift.
                    val order = (0 until list.childCount).map { list.getChildAt(it) }
                    val layoutTop = order.takeWhile { it !== row }.sumOf { it.height }
                    row.translationY = anchorTop + dy - layoutTop
                    return true
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                    list.parent?.requestDisallowInterceptTouchEvent(false)
                    row.animate().translationY(0f).alpha(1f).setDuration(120).start()
                    row.elevation = 0f
                    if (moved && e.actionMasked == MotionEvent.ACTION_UP) {
                        onDone((0 until list.childCount).map { list.getChildAt(it).tag as Long })
                    }
                    return true
                }
            }
            return false
        }
    }
}
