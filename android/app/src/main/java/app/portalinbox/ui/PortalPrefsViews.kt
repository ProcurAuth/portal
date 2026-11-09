package app.portalinbox.ui

import android.content.Context
import android.content.res.ColorStateList
import android.view.Gravity
import android.view.View
import android.widget.CheckBox
import android.widget.LinearLayout
import android.widget.Switch
import app.portalinbox.R
import app.portalinbox.portal.PrefsApply
import app.portalinbox.portal.PortalIdentity

/*
 * Views of the settings of one portal (lot MP5): a row with a switch, and a line of the apply-to-all sheet. They only draw. The
 * decisions are in [app.portalinbox.ui.PrefsForm] and [PrefsApply].
 */

/**
 * A row with a title, a help line and a switch at the end. The whole row toggles the switch, and the switch carries the spoken
 * label, so TalkBack reads one control. [onToggle] runs when the user changes it, never when the code sets the first value (the
 * listener is set after the value).
 *
 * With [onOpen] the row is a button of its own (it opens an editor) and the switch is a second control with the label
 * [switchLabel]: a tap on the text edits, a tap on the switch toggles.
 */
fun Context.switchRow(
    title: String,
    help: String?,
    checked: Boolean,
    onToggle: (Boolean) -> Unit,
    onOpen: (() -> Unit)? = null,
    switchLabel: String = title,
): LinearLayout {
    val sw = Switch(this).apply {
        isChecked = checked
        showText = false
        contentDescription = if (onOpen != null) switchLabel else if (help == null) title else "$title. $help"
        val on = intArrayOf(android.R.attr.state_checked)
        thumbTintList = ColorStateList(arrayOf(on, intArrayOf()), intArrayOf(color(R.color.primary), color(R.color.text_muted)))
        trackTintList = ColorStateList(arrayOf(on, intArrayOf()), intArrayOf(color(R.color.primary_soft), color(R.color.field)))
        setOnCheckedChangeListener { _, value -> onToggle(value) }
    }
    return lin(vertical = false, padH = 14, padV = if (onOpen == null) 10 else 0).apply {
        minimumHeight = dp(56)
        gravity = Gravity.CENTER_VERTICAL
        val texts = lin()
        texts.addView(label(title, SP_M, color(R.color.text), MEDIUM))
        help?.let { texts.addView(label(it, SP_S, color(R.color.text_muted)), lp().margins(this@switchRow, t = 2)) }
        if (onOpen == null) {
            texts.importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS
        } else {
            texts.apply {
                // The text is a button of its own: its target is the full height of the row, 56 dp.
                minimumHeight = dp(56)
                setPadding(0, dp(10), 0, dp(10))
                gravity = Gravity.CENTER_VERTICAL
                background = rowRipple()
                isClickable = true
                isFocusable = true
                contentDescription = if (help == null) title else "$title. $help"
                setOnClickListener { onOpen() }
            }
        }
        addView(texts, lp(0, WRAP, 1f))
        addView(sw, lp(WRAP, WRAP).margins(this@switchRow, l = 10))
        if (onOpen == null) {
            background = rowRipple()
            isClickable = true
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
            setOnClickListener { sw.toggle() }
        }
    }
}

/** The check box of a line of the apply-to-all sheet. A line that cannot send shows an unticked box that is off. */
private fun Context.sheetCheck(on: Boolean, enabled: Boolean, description: String, onChange: (Boolean) -> Unit): CheckBox = CheckBox(this).apply {
    isChecked = on
    isEnabled = enabled
    contentDescription = description
    buttonTintList = ColorStateList.valueOf(color(R.color.primary))
    setOnCheckedChangeListener { _, v -> onChange(v) }
}

/**
 * One line of the apply-to-all sheet: the tick, the monogram, the nickname and what will happen. A line that cannot send is dim.
 * The text under the name is in the color of its state: muted when it sends now, warning when it waits or cannot send.
 */
fun Context.applyLine(identity: PortalIdentity, step: PrefsApply.Step, ticked: Boolean, onTick: (Boolean) -> Unit): LinearLayout {
    val box = sheetCheck(ticked, step.selectable, "${identity.nickname}. ${step.text}", onTick)
    return lin(vertical = false, padH = 12, padV = 8).apply {
        minimumHeight = dp(56)
        gravity = Gravity.CENTER_VERTICAL
        alpha = if (step.selectable) 1f else 0.7f
        addView(box, lp(WRAP, WRAP).margins(this@applyLine, r = 6))
        addView(monogramSquare(identity, 32), lp(dp(32), dp(32)).margins(this@applyLine, r = 10))
        val texts = lin()
        texts.addView(label(identity.nickname, SP_M, color(R.color.text_strong), MEDIUM, lines = 1))
        val fg = if (step.reason == PrefsApply.Reason.NOW || step.reason == PrefsApply.Reason.SAME) R.color.text_muted else R.color.warning
        texts.addView(label(step.text, SP_S, color(fg)), lp().margins(this@applyLine, t = 2))
        step.accountText?.let { texts.addView(label(it, SP_S, color(R.color.text_muted)), lp().margins(this@applyLine, t = 2)) }
        addView(texts, lp(0, WRAP, 1f))
        if (step.selectable) {
            background = rowRipple()
            isClickable = true
            setOnClickListener { box.toggle() }
        }
    }
}
