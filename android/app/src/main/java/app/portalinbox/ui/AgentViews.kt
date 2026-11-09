package app.portalinbox.ui

import android.content.Context
import android.graphics.Typeface
import android.view.Gravity
import android.view.View
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.proto.AgentKind
import app.portalinbox.proto.AgentState

/**
 * The view parts that the agent screens share (lot G4): the state chip, the box of hints, the header of a group. The decisions are in the
 * pure classes ([AgentGroups], [NewAgentCard], [AgentScreenModel], [FromAgentBox]); this file only draws. Each text that a client sent goes
 * through [UntrustedText] and `SpanApplier`: a hidden or look-alike character shows as a mark.
 */

/** A text of a client, marked, in a text view. The view is never selectable and has no link. */
fun Context.untrustedLabel(text: String, size: Float = SP_S, color: Int = color(R.color.text), quoted: Boolean = false, italic: Boolean = false): TextView =
    label(SpanApplier(this).apply(if (quoted) UntrustedText.quoted(text) else UntrustedText.mark(text)), size, color).apply {
        setTextIsSelectable(false)
        isLongClickable = false
        movementMethod = null
        if (italic) setTypeface(REGULAR, Typeface.ITALIC)
    }

/** The chip of the state: NEW in amber, KNOWN in the primary color, BLOCKED in red, the others muted. The word is always written. */
fun Context.agentStateChip(state: AgentState, kind: AgentKind): TextView {
    val (fg, bg) = when {
        kind == AgentKind.BUCKET -> color(R.color.warning) to color(R.color.warning_soft)
        state == AgentState.NEW -> color(R.color.warning) to color(R.color.warning_soft)
        state == AgentState.KNOWN -> color(R.color.primary) to color(R.color.primary_soft)
        state == AgentState.BLOCKED -> color(R.color.danger) to color(R.color.danger_soft)
        else -> color(R.color.text_muted) to color(R.color.bg_hover)
    }
    return chip(AgentTexts.stateWord(state, kind), fg, bg).apply { contentDescription = AgentTexts.stateSpoken(state, kind) }
}

/** The chip of the key name: a mono tag. Text of the portal. */
fun Context.keyChip(keyName: String): TextView =
    mono(VisibleText.escapeLine(keyName), SP_XS, color(R.color.text_muted)).apply {
        background = rounded(color(R.color.field), dpf(CHIP_RADIUS), color(R.color.glass_stroke), dp(1))
        setPadding(dp(6), dp(3), dp(6), dp(3))
    }

/** A dashed horizontal line, the look of the text of a third party. */
fun Context.dashedLine(): View = View(this).apply {
    background = android.graphics.drawable.GradientDrawable().apply { setStroke(dp(1), color(R.color.card_stroke), dpf(4f), dpf(3f)) }
    setLayerType(View.LAYER_TYPE_SOFTWARE, null)
    layoutParams = LinearLayout.LayoutParams(MATCH, dp(1))
}

/**
 * The box "Observed, not verified": a dashed edge, the rows of hints, the warnings in amber, and the sentence that says that these
 * texts are no proof. [rows] have our label and the text of a client.
 */
fun Context.observedBox(rows: List<AgentTexts.ObservedRow>, warnings: List<String>, noProof: String?): View {
    val box = lin().apply {
        background = dashedBox(color(R.color.card), color(R.color.card_stroke), dpf(CARD_RADIUS))
        clipToOutline = true
        outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
    }
    val head = lin(vertical = false, padH = 12, padV = 8).apply { gravity = Gravity.CENTER_VERTICAL }
    head.addView(icon(R.drawable.ic_warning, color(R.color.warning), 16), lp(dp(16), dp(16)).margins(this, r = 8))
    head.addView(caption(getString(R.string.agent_observed_not_verified), color(R.color.warning)), lp(0, WRAP, 1f))
    box.addView(head)
    if (rows.isNotEmpty() || warnings.isNotEmpty() || noProof != null) box.addView(dashedLine())
    if (rows.isNotEmpty()) {
        val list = lin(padH = 12, padV = 8)
        for (r in rows) {
            val line = lin(vertical = false).apply { gravity = Gravity.TOP }
            line.addView(label(r.label, SP_S, color(R.color.text_muted)), lp(dp(96), WRAP))
            line.addView(untrustedLabel(r.value, SP_S, color(R.color.text)), lp(0, WRAP, 1f))
            list.addView(line, lp().margins(this, t = 2, b = 2))
        }
        box.addView(list)
    }
    if (warnings.isNotEmpty()) {
        val w = lin(padH = 12, padV = 4)
        for (line in warnings) w.addView(label(line, SP_S, color(R.color.warning), MEDIUM), lp().margins(this, t = 2, b = 2))
        box.addView(w)
    }
    if (noProof != null) {
        box.addView(dashedLine())
        box.addView(label(noProof, SP_S, color(R.color.text_muted)).apply { setPadding(dp(12), dp(8), dp(12), dp(8)) })
    }
    return box
}

/** A one-line field for a name. The draft stays in the caller's map, so a refresh of the screen never loses the text. */
fun Context.nameField(hint: String, draft: String, onChange: (String) -> Unit): EditText = EditText(this).apply {
    this.hint = hint
    setText(draft)
    setSingleLine()
    textSize = SP_M
    setTextColor(color(R.color.text))
    setHintTextColor(color(R.color.text_subtle))
    background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
    setPadding(dp(12), dp(12), dp(12), dp(12))
    minHeight = dp(48)
    inputType = android.text.InputType.TYPE_CLASS_TEXT
    filters = arrayOf(android.text.InputFilter.LengthFilter(app.portalinbox.proto.MAX_AGENT_NAME_CHARS * 2))
    addTextChangedListener(object : android.text.TextWatcher {
        override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
        override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
        override fun afterTextChanged(s: android.text.Editable?) = onChange(s?.toString() ?: "")
    })
}

/**
 * The sheet that asks which agent to merge into. [choices] are the live agents of the same key. The agent that the portal named as
 * "looks like" comes first and says so. A tap on a row sends the merge. The words warn that a merge cannot be undone.
 */
fun android.app.Activity.agentMergeSheet(choices: List<NewAgentCard.MergeChoice>, onPick: (String) -> Unit) {
    bottomSheet { sheet, dialog ->
        sheet.addView(label(getString(R.string.agent_merge_picker_title), SP_L, color(R.color.text_strong), BOLD))
        sheet.addView(label(getString(R.string.agent_merge_picker_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4, b = 8))
        val panel = panel()
        choices.forEachIndexed { i, c ->
            if (i > 0) panel.addView(divider())
            val mark = if (c.looksLike) label(getString(R.string.agent_looks_like_mark), SP_S, color(R.color.warning), MEDIUM) else null
            panel.addView(row(c.label, null, mark) {
                dialog.dismiss()
                onPick(c.agentId)
            }.apply { minimumHeight = dp(52) })
        }
        sheet.addView(panel)
        sheet.addView(button(getString(R.string.nav_back), ButtonKind.SECONDARY) { dialog.dismiss() }, lp().margins(this, t = 12, b = 8))
    }
}

/** The sheet that asks before a block. A block cancels the open requests of the agent, and they do not come back. */
fun android.app.Activity.agentBlockSheet(onConfirm: () -> Unit) {
    bottomSheet { sheet, dialog ->
        sheet.addView(label(getString(R.string.agent_block_confirm_title), SP_L, color(R.color.text_strong), BOLD))
        sheet.addView(label(getString(R.string.agent_block_confirm_body), SP_M, color(R.color.text_muted)), lp().margins(this, t = 6, b = 12))
        sheet.addView(button(getString(R.string.agent_block), ButtonKind.DANGER) {
            dialog.dismiss()
            onConfirm()
        })
        sheet.addView(button(getString(R.string.nav_back), ButtonKind.SECONDARY) { dialog.dismiss() }, lp().margins(this, t = 8, b = 8))
    }
}

/**
 * The box "From agent" under the title of the approval screen. The name, the state chip and the key chip are text of the portal. The
 * dashed part holds what the agent says, marked "Not verified", and the warnings of a new agent. [onOpen] opens the agent screen.
 */
fun Context.fromAgentBox(b: FromAgentBox.Box, onOpen: () -> Unit): View {
    val card = lin(padH = 12, padV = 10).apply { background = solidCard() }
    card.addView(caption(getString(R.string.agent_from), color(R.color.text_muted)))
    val head = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
    head.addView(label(b.name, SP_XL, color(R.color.text_strong), BOLD).apply {
        if (android.os.Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
    }, lp(WRAP, WRAP))
    head.addView(agentStateChip(b.agent?.state ?: if (b.isNew) AgentState.NEW else AgentState.KNOWN, if (b.isBucket) AgentKind.BUCKET else AgentKind.AGENT), lp(WRAP, WRAP).margins(this, l = 8))
    head.addView(keyChip(b.keyName), lp(WRAP, WRAP).margins(this, l = 6))
    card.addView(head, lp().margins(this, t = 4))
    card.contentDescription = b.name + ", key " + b.keyName + ", " + b.stateSpoken
    if (b.isBucket) {
        card.addView(label(getString(R.string.agent_unattributed_note), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
    } else if (b.says != null || b.warnings.isNotEmpty()) {
        val dashed = lin(padH = 10, padV = 8).apply { background = dashedBox(color(R.color.card), color(R.color.card_stroke), dpf(CARD_RADIUS)) }
        val top = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        top.addView(caption(getString(R.string.agent_not_verified), color(R.color.warning)), lp(WRAP, WRAP).margins(this, r = 8))
        b.says?.let { top.addView(untrustedSays(getString(R.string.agent_says_it_is), it), lp(0, WRAP, 1f)) }
        dashed.addView(top)
        b.saysHere?.let { dashed.addView(untrustedSays(getString(R.string.agent_says_here), it), lp().margins(this, t = 4)) }
        for (w in b.warnings) dashed.addView(label(w, SP_S, color(R.color.warning), MEDIUM), lp().margins(this, t = 4))
        card.addView(dashed, lp().margins(this, t = 8))
    }
    if (b.canOpenAgent) {
        card.addView(label(getString(R.string.agent_open), SP_M, color(R.color.primary), MEDIUM).apply {
            minHeight = dp(48)
            gravity = Gravity.CENTER_VERTICAL
            isClickable = true
            isFocusable = true
            background = rowRipple()
            setOnClickListener { onOpen() }
        }, lp().margins(this, t = 4))
    }
    return card
}

/** "Says it is "billing-bot"": our words, then the text of the agent between quotation marks, marked. */
private fun Context.untrustedSays(prefix: String, text: String): TextView {
    val marked = UntrustedText.quoted(text)
    val applied = android.text.SpannableStringBuilder("$prefix ").append(SpanApplier(this).apply(marked))
    return label(applied, SP_S, color(R.color.text)).apply {
        setTypeface(REGULAR, Typeface.ITALIC)
        setTextIsSelectable(false)
        isLongClickable = false
        movementMethod = null
    }
}
