package app.portalinbox.ui

import android.content.Context
import android.view.Gravity
import android.view.View
import android.widget.EditText
import android.widget.LinearLayout
import app.portalinbox.R
import app.portalinbox.portal.HomePortal

/**
 * The views of the home list for the agents: the header of a group and the card "New agent". They only draw. The facts come
 * from [AgentGroups.Group] and [NewAgentCard.Model]. A tap on the header folds the group. A tap on "More" opens the agent screen.
 */

/** What the owner can do on the card of a new agent. The caller sends the frame and shows the answer. */
interface NewAgentActions {
    fun nameAndAllow(m: NewAgentCard.Model, name: String)
    fun allowAsPortalName(m: NewAgentCard.Model)
    fun block(m: NewAgentCard.Model)
    fun merge(m: NewAgentCard.Model)
}

/**
 * The header of a group. Chevron, name, "key . last call", the hint line, the count and "More". The background is amber for a new
 * agent and dashed for the bucket. [portal] is set when the phone holds several portals: the chip names the portal, as on each row.
 */
fun Context.agentGroupHeader(g: AgentGroups.Group, nowMs: Long, portal: HomePortal?, onToggle: () -> Unit, onMore: () -> Unit): View {
    val box = lin()
    val top = lin(vertical = false, padH = 12, padV = 8).apply {
        minimumHeight = dp(54)
        background = rowRipple()
        isClickable = true
        isFocusable = true
        contentDescription = AgentGroups.spoken(g, nowMs)
        setOnClickListener { onToggle() }
    }
    top.addView(icon(R.drawable.ic_chevron_right, color(R.color.text_muted), 16).apply { rotation = if (g.folded) 0f else 90f }, lp(dp(16), dp(16)).margins(this, r = 10))
    val names = lin()
    val nameLine = lin(vertical = false)
    nameLine.addView(label(g.name, SP_L, color(R.color.text_strong), BOLD, lines = 1), lp(WRAP, WRAP))
    portal?.let { nameLine.addView(portalChip(it), lp(WRAP, WRAP).margins(this, l = 8)) }
    names.addView(nameLine)
    names.addView(label(AgentGroups.keyLine(g, nowMs), SP_S, color(R.color.text_muted), lines = 1), lp().margins(this, t = 2))
    top.addView(names, lp(0, WRAP, 1f))
    top.addView(label(g.count.toString(), SP_S, color(R.color.primary), MEDIUM).apply {
        gravity = Gravity.CENTER
        minWidth = dp(26)
        background = rounded(color(R.color.primary_soft), dpf(13f))
        setPadding(dp(7), dp(3), dp(7), dp(3))
    }, lp(WRAP, WRAP).margins(this, l = 8))
    top.addView(label(getString(R.string.agent_more), SP_S, color(R.color.primary), MEDIUM).apply {
        gravity = Gravity.CENTER
        minHeight = dp(48)
        setPadding(dp(10), 0, dp(4), 0)
        isClickable = true
        isFocusable = true
        contentDescription = getString(R.string.agent_more_description)
        setOnClickListener { onMore() }
    }, lp(WRAP, WRAP))
    box.addView(top)
    AgentGroups.hint(g)?.let { h ->
        box.addView(dashedLine())
        val line = lin(vertical = false, padH = 12, padV = 5).apply { gravity = Gravity.CENTER_VERTICAL }
        line.addView(caption(h.label, color(R.color.warning)), lp(WRAP, WRAP).margins(this, r = 10))
        val text = h.parts.joinToString(" · ")
        line.addView(
            if (h.untrusted) {
                // Each part is marked alone: the separator of the portal is not a mark.
                label(SpanApplier(this).apply(UntrustedText.joined(h.parts, " · ")), SP_S, color(R.color.text_muted)).apply {
                    setTextIsSelectable(false)
                    isLongClickable = false
                    movementMethod = null
                }
            } else label(text, SP_S, color(R.color.text_muted)),
            lp(0, WRAP, 1f),
        )
        box.addView(line)
    }
    // A hairline between the header and the rows. A folded group has no row under it.
    if (!g.folded) box.addView(divider())
    return box
}

/** The line under the rows of a new agent that the server locks: a lock and the words. */
fun Context.agentLockLine(text: String): View {
    val row = lin(vertical = false, padH = 12, padV = 8).apply {
        setBackgroundColor(color(R.color.warning_soft))
    }
    row.addView(icon(R.drawable.ic_lock, color(R.color.warning), 16), lp(dp(16), dp(16)).margins(this, r = 8))
    row.addView(label(text, SP_S, color(R.color.warning), MEDIUM), lp(0, WRAP, 1f))
    return row
}

/**
 * The card "New agent": the title, the first call, the box of hints, the name field, and the four choices. [draft] is the text that the
 * owner already typed. [onDraft] keeps it. [onFocus] tells the screen that the owner types now, so that it does not redraw under the keyboard.
 */
fun Context.newAgentCard(
    m: NewAgentCard.Model, draft: String, onDraft: (String) -> Unit, onFocus: (Boolean) -> Unit, actions: NewAgentActions,
): View {
    val card = lin().apply {
        background = rounded(color(R.color.warning_soft), dpf(CARD_RADIUS), color(R.color.warning) and 0x66FFFFFF, dp(1))
        clipToOutline = true
        outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
    }
    val head = lin(vertical = false, padH = 12, padV = 10).apply { gravity = Gravity.CENTER_VERTICAL }
    head.addView(icon(R.drawable.ic_warning, color(R.color.warning), 20), lp(dp(20), dp(20)).margins(this, r = 10))
    val titles = lin()
    titles.addView(label(m.title, SP_L, color(R.color.text_strong), BOLD).apply {
        if (android.os.Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
    })
    titles.addView(label(m.subtitle, SP_S, color(R.color.text_muted)), lp().margins(this, t = 2))
    head.addView(titles, lp(0, WRAP, 1f))
    card.addView(head)

    val body = lin(padH = 12, padV = 10).apply { setBackgroundColor(color(R.color.card)) }
    body.addView(observedBox(m.observed, m.warnings, m.noProof))
    body.addView(caption(getString(R.string.agent_name_for), color(R.color.text_muted)), lp().margins(this, t = 12, b = 6))
    val sending = m.pending.isNotEmpty()
    val field: EditText = nameField(getString(R.string.agent_name_hint), draft, onDraft).apply {
        setOnFocusChangeListener { _, has -> onFocus(has) }
        isEnabled = !sending
    }
    body.addView(field, lp())
    val primary = button(getString(R.string.agent_name_and_allow), ButtonKind.PRIMARY) { actions.nameAndAllow(m, field.text.toString()) }
    body.addView(primary, lp().margins(this, t = 10))
    val two = lin(vertical = false)
    val allowAs = button(m.allowAsLabel, ButtonKind.SECONDARY) { actions.allowAsPortalName(m) }
    val block = button(getString(R.string.agent_block), ButtonKind.DANGER) { actions.block(m) }
    two.addView(allowAs, lp(0, WRAP, 1f).margins(this, r = 6))
    two.addView(block, lp(0, WRAP, 1f).margins(this, l = 6))
    body.addView(two, lp().margins(this, t = 8))
    val mergeButton = if (m.canMerge) button(m.mergeLabel, ButtonKind.SECONDARY) { actions.merge(m) } else null
    mergeButton?.let { body.addView(it, lp().margins(this, t = 8)) }
    if (sending) {
        for (v in listOfNotNull(primary, allowAs, block, mergeButton)) {
            v.alpha = 0.45f
            v.isEnabled = false
            v.isClickable = false
        }
        body.addView(label(getString(R.string.agent_sending), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
    }
    card.addView(body)
    m.lockLine?.let { card.addView(agentLockLine(it)) }
    return card
}
