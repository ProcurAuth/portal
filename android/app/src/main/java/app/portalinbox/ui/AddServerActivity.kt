package app.portalinbox.ui

import android.content.Intent
import android.content.res.ColorStateList
import android.content.res.Configuration
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.data.KeyVault
import app.portalinbox.data.Store
import app.portalinbox.net.DeepLink
import app.portalinbox.net.EnrollCodeInfo
import app.portalinbox.net.Enroller
import app.portalinbox.net.EnrollmentError
import app.portalinbox.portal.EnrollPlan
import app.portalinbox.portal.FingerprintText
import app.portalinbox.portal.KnownPortal
import app.portalinbox.portal.LookAlike
import app.portalinbox.portal.PortalAdmission
import app.portalinbox.portal.PortalNaming
import app.portalinbox.service.InboxService
import app.portalinbox.service.Registry

/**
 * Enrolls this phone with a one-time enrollment code made by an account of the portal. Two steps (proposal 3.5):
 * 1. The code step: the user types or pastes the code. A tapped `portal-inbox://enroll?...` link skips it. Nothing is used.
 * 2. The confirmation step: the address (with the changed letters of a look-alike marked), the full fingerprint, the name and the
 *    color of the portal on this phone, and the tick. The code is used only when the user taps "Add portal".
 *
 * Everything that this screen shows or requires comes from [EnrollPlan]. The name is never proposed and never taken from the
 * portal. This class is exported for the link, so it never starts in re-enroll mode: [ReenrollActivity] (not exported) does.
 * The code and the link stay in memory only.
 */
open class AddServerActivity : BaseActivity() {
    /** The id of the portal to enroll again, or null for a new portal. Only [ReenrollActivity] answers with an id. */
    protected open fun reenrollId(): Long? = null

    private var target: KnownPortal? = null
    private var codeText: String? = null
    private var codeDraft = ""
    private var info: EnrollCodeInfo? = null
    private var source: EnrollPlan.Source = EnrollPlan.Source.Typed
    private var nickname = ""
    private var colorChoice: Int? = null
    private var ticked = false
    private var tickReason: EnrollPlan.TickReason? = null
    private var tickBuilt = false
    private var deviceName: String = Build.MODEL ?: ""
    private var codeError: String? = null
    private var busy = false
    private var plan: EnrollPlan.Plan? = null

    private lateinit var holder: LinearLayout

    // Views of the confirmation step.
    private var bannerBox: LinearLayout? = null
    private var addressText: TextView? = null
    private var newKeyText: TextView? = null
    private var nameError: TextView? = null
    private var nameCount: TextView? = null
    private var pickerBox: LinearLayout? = null
    private var tickBox: LinearLayout? = null
    private var addButton: TextView? = null
    private var statusText: TextView? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val id = reenrollId()
        if (id != null) {
            target = Store.get(this).knownPortals().firstOrNull { it.identity.id == id } ?: return finish()
        }
        title = getString(if (target == null) R.string.add_title else R.string.enroll_reenroll_title)
        holder = lin()
        setScreen(holder)
        showCodeStep()
        prefill(intent, first = true)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        prefill(intent, first = false)
    }

    /** The layout depends on the width: the step is drawn again. The state is in the fields of this class. */
    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        if (info != null) showConfirmStep() else showCodeStep()
    }

    override fun onDestroy() {
        // The code is a one-time secret: it does not outlive the screen.
        codeText = null
        codeDraft = ""
        super.onDestroy()
    }

    // ------------------------------------------------------------ the link

    /** `portal-inbox://enroll?...`: reads the code and shows the confirmation. It uses nothing: the user still confirms. A link never gives re-enroll mode. */
    private fun prefill(intent: Intent?, first: Boolean) {
        val data = intent?.data ?: return
        if (target != null || !DeepLink.isEnroll(data.scheme, data.host)) return
        resetEntry()
        val text = data.toString()
        val parsed = try { Enroller.parse(this, text) } catch (e: EnrollmentError) { null }
        if (parsed == null) {
            codeDraft = text
            codeError = getString(R.string.err_code_invalid)
            showCodeStep()
            return
        }
        codeText = text
        info = parsed
        source = EnrollPlan.Source.Link(referrerOf(intent, first))
        showConfirmStep()
    }

    /** What Android says about who opened the link, as a URI text. A hint, never proof: the sender can set it. */
    private fun referrerOf(intent: Intent, first: Boolean): String? {
        val extra: Uri? = if (Build.VERSION.SDK_INT >= 33) {
            intent.getParcelableExtra(Intent.EXTRA_REFERRER, Uri::class.java)
        } else {
            @Suppress("DEPRECATION") intent.getParcelableExtra<Uri>(Intent.EXTRA_REFERRER)
        }
        return extra?.toString() ?: intent.getStringExtra(Intent.EXTRA_REFERRER_NAME) ?: if (first) referrer?.toString() else null
    }

    /** "Chrome (com.android.chrome)": the label of the app that sent the link, and its package. The label is the choice of that app, so the package always shows. */
    private fun referrerLine(hint: String?): String {
        if (hint == null) return getString(R.string.enroll_opened_unknown)
        val label = try {
            packageManager.getApplicationInfo(hint, 0).loadLabel(packageManager).toString()
        } catch (e: Exception) {
            null
        }
        val shown = if (label != null && label != hint) "${VisibleText.escapeLine(label.take(40))} ($hint)" else hint
        return getString(R.string.enroll_opened_from, shown)
    }

    private fun resetEntry() {
        nickname = ""
        colorChoice = null
        ticked = false
        tickReason = null
        codeError = null
        plan = null
    }

    // ------------------------------------------------------------ step 1: the code

    private fun showCodeStep() {
        info = null
        holder.removeAllViews()
        holder.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }))
        val body = lin(padH = 10, padV = 4)
        holder.addView(scroll(body), lp(MATCH, 0, 1f))

        val head = target
        if (head != null) {
            body.addView(sectionTitle(getString(R.string.enroll_reenroll_head, head.identity.nickname)))
            body.addView(identityLine(head), lp().margins(this, l = 4, b = 6))
            body.addView(label(getString(R.string.enroll_reenroll_help), SP_M, color(R.color.text_muted)).apply { setPadding(dp(4), 0, dp(4), dp(8)) })
        } else {
            body.addView(sectionTitle(getString(R.string.add_title)))
        }

        val form = panel(padH = 14, padV = 12)
        val code = textField(codeDraft, getString(R.string.add_code_hint), mono = true).apply {
            id = View.generateViewId()
            isSingleLine = false
            minLines = 2
            maxLines = 5
            gravity = Gravity.TOP or Gravity.START
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or
                InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or InputType.TYPE_TEXT_VARIATION_URI
            imeOptions = EditorInfo.IME_ACTION_DONE
            textSize = SP_S
        }
        form.addView(label(getString(R.string.add_code), SP_S, color(R.color.text), MEDIUM).apply { labelFor = code.id })
        form.addView(label(getString(R.string.enroll_code_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 2))
        form.addView(code, lp().margins(this, t = 6))
        val error = label(codeError.orEmpty(), SP_S, color(R.color.danger)).apply {
            visibility = if (codeError == null) View.GONE else View.VISIBLE
            accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
        }
        form.addView(error, lp().margins(this, t = 4))
        body.addView(form)

        body.addView(lin(vertical = false, padH = 4, padV = 10).apply {
            addView(icon(R.drawable.ic_info, color(R.color.text_muted), 16), lp(dp(16), dp(16)).margins(this@AddServerActivity, r = 8))
            addView(label(getString(R.string.enroll_secret_note), SP_S, color(R.color.text_muted)), lp(0, WRAP, 1f))
        })

        fun next() {
            codeDraft = code.text.toString()
            val text = codeDraft.trim()
            if (text.isEmpty()) { error.text = getString(R.string.add_code_missing); error.visibility = View.VISIBLE; return }
            val parsed = try { Enroller.parse(this, text) } catch (e: EnrollmentError) {
                error.text = e.message ?: getString(R.string.err_code_invalid); error.visibility = View.VISIBLE; return
            }
            resetEntry()
            codeText = text
            info = parsed
            source = EnrollPlan.Source.Typed
            showConfirmStep()
        }
        code.setOnEditorActionListener { _, action, _ -> if (action == EditorInfo.IME_ACTION_DONE) { next(); true } else false }
        code.addTextChangedListener(simpleWatcher { codeDraft = it; if (error.visibility == View.VISIBLE) error.visibility = View.GONE })
        body.addView(button(getString(R.string.enroll_continue)) { next() }, lp().margins(this, t = 4, b = 16))
    }

    // ------------------------------------------------------------ step 2: the confirmation

    private fun showConfirmStep() {
        val i = info ?: return showCodeStep()
        holder.removeAllViews()
        holder.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { showOrFinish() }))
        val body = lin(padH = 10, padV = 4)
        holder.addView(scroll(body), lp(MATCH, 0, 1f))
        val reenroll = target

        body.addView(sectionTitle(getString(if (reenroll == null) R.string.add_title else R.string.enroll_reenroll_title)))
        bannerBox = lin().also { body.addView(it) }

        // Address and key.
        val card = panel(padH = 14, padV = 12)
        card.addView(label(getString(R.string.enroll_address).uppercase(), SP_XS, color(R.color.text_muted), MEDIUM))
        addressText = mono("", SP_L, color(R.color.text_strong), bold = true, lines = 3).also { card.addView(it, lp().margins(this, t = 4)) }
        card.addView(label(getString(R.string.enroll_key).uppercase(), SP_XS, color(R.color.text_muted), MEDIUM), lp().margins(this, t = 12))
        if (reenroll != null) {
            card.addView(label(getString(R.string.enroll_key_before), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4))
            card.addView(mono(FingerprintText.lines(reenroll.identity.fingerprint), SP_L, color(R.color.text), lines = 2), lp().margins(this, t = 2))
            card.addView(label(getString(R.string.enroll_key_new), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
        }
        newKeyText = mono(FingerprintText.lines(i.fingerprint), SP_L, color(R.color.text_strong), bold = true, lines = 2).also {
            it.contentDescription = FingerprintText.spoken(i.fingerprint)
            card.addView(it, lp().margins(this, t = 4))
        }
        card.addView(label(getString(R.string.enroll_key_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
        body.addView(card, lp().margins(this, t = 8))

        if (reenroll == null) {
            // Name: required, never proposed.
            body.addView(label(getString(R.string.enroll_name), SP_M, color(R.color.text), MEDIUM).apply { setPadding(dp(4), 0, 0, 0) }, lp().margins(this, t = 16))
            body.addView(label(getString(R.string.enroll_name_note), SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), 0, 0, 0) }, lp().margins(this, t = 2))
            val name = textField(nickname, "").apply {
                id = View.generateViewId()
                imeOptions = EditorInfo.IME_ACTION_DONE
                inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
                contentDescription = getString(R.string.enroll_name)
            }
            name.addTextChangedListener(simpleWatcher { nickname = it; update() })
            body.addView(name, lp().margins(this, t = 6))
            val under = lin(vertical = false)
            nameError = label("", SP_S, color(R.color.danger)).apply { accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE }
            nameCount = label("", SP_S, color(R.color.text_muted)).apply { gravity = Gravity.END }
            under.addView(nameError, lp(0, WRAP, 1f))
            under.addView(nameCount, lp(WRAP, WRAP))
            body.addView(under, lp().margins(this, l = 4, t = 4, r = 4))

            body.addView(label(getString(R.string.enroll_color), SP_M, color(R.color.text), MEDIUM).apply { setPadding(dp(4), 0, 0, 0) }, lp().margins(this, t = 14))
            pickerBox = lin().also { body.addView(it, lp().margins(this, t = 8)) }
        } else {
            body.addView(label(getString(R.string.enroll_name_fixed), SP_M, color(R.color.text), MEDIUM).apply { setPadding(dp(4), 0, 0, 0) }, lp().margins(this, t = 16))
            body.addView(identityLine(reenroll), lp().margins(this, l = 4, t = 6))
        }

        tickBox = lin().also { body.addView(it, lp().margins(this, t = 14)) }
        tickBuilt = false

        // The device name: small, after the rest. The portal shows it in its web interface.
        body.addView(label(getString(R.string.add_device_name), SP_S, color(R.color.text), MEDIUM).apply { setPadding(dp(4), 0, 0, 0) }, lp().margins(this, t = 18))
        body.addView(label(getString(R.string.enroll_device_help), SP_S, color(R.color.text_muted)).apply { setPadding(dp(4), 0, 0, 0) }, lp().margins(this, t = 2))
        val device = textField(deviceName, "").apply { id = View.generateViewId(); inputType = InputType.TYPE_CLASS_TEXT }
        device.addTextChangedListener(simpleWatcher { deviceName = it; update() })
        body.addView(device, lp().margins(this, t = 6))
        statusText = label("", SP_S, color(R.color.danger)).apply {
            setPadding(dp(4), dp(10), dp(4), dp(10))
            visibility = View.GONE
            accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
        }.also { body.addView(it) }

        // The buttons stay at the bottom.
        val bar = lin(vertical = false, padH = 10, padV = 8)
        bar.addView(button(getString(R.string.cancel), ButtonKind.SECONDARY) { finish() }, lp(0, WRAP, 1f))
        addButton = button(getString(R.string.enroll_add)) { submit() }.also { bar.addView(it, lp(0, WRAP, 1.4f).margins(this, l = 10)) }
        holder.addView(bar)

        rebuildPicker()
        update()
    }

    /** The back arrow of the confirmation: a typed code returns to the code step, a link closes the screen. */
    private fun showOrFinish() {
        if (source is EnrollPlan.Source.Link) finish() else {
            info = null
            codeText = null
            showCodeStep()
        }
    }

    private fun computePlan(): EnrollPlan.Plan? {
        val i = info ?: return null
        val mode = target?.let { EnrollPlan.Mode.Reenroll(it) } ?: EnrollPlan.Mode.Add
        return EnrollPlan.plan(
            EnrollPlan.Code(i.address.http, i.fingerprint), source, mode, Store.get(this).knownPortals(),
            EnrollPlan.Entry(nickname, colorChoice, ticked),
        )
    }

    /** Draws the parts of the screen that depend on what the user typed. The fields themselves are not touched, so the keyboard stays. */
    private fun update() {
        val p = computePlan() ?: return
        plan = p
        val reenroll = target != null

        addressText?.text = markedAddress(p.shownAddress, p.changedRanges)
        bannerBox?.let { box -> box.removeAllViews(); fillBanners(box, p) }

        // Name.
        val check = p.nickname
        val typed = nickname
        nameCount?.apply {
            val n = typed.codePointCount(0, typed.length)
            text = if (typed.isEmpty()) "" else getString(R.string.enroll_name_count, n, PortalNaming.MAX_LENGTH)
            setTextColor(color(if (n > PortalNaming.MAX_LENGTH) R.color.danger else R.color.text_muted))
        }
        nameError?.text = if (check is PortalNaming.Check.Invalid && typed.isNotEmpty()) nameProblem(check) else ""

        // The tick. A tick given for one reason does not count for another one.
        val want = p.tickReason
        if (want != tickReason) {
            ticked = EnrollPlan.tickAfter(tickReason, want, ticked)
            tickReason = want
            tickBuilt = false
        }
        tickBox?.let { box ->
            if (!tickBuilt) {
                tickBuilt = true
                box.removeAllViews()
                if (want != null) {
                    val tick = CheckBox(this).apply {
                        text = getString(if (reenroll) R.string.enroll_tick_reenroll else R.string.enroll_tick)
                        textSize = SP_M
                        minHeight = dp(48)
                        isChecked = ticked
                        setTextColor(color(R.color.text))
                        buttonTintList = ColorStateList.valueOf(color(R.color.primary))
                        setPadding(dp(8), 0, dp(10), 0)
                        setOnCheckedChangeListener { _, on -> ticked = on; update() }
                    }
                    box.addView(tick)
                    val why = when (want) {
                        EnrollPlan.TickReason.REENROLL -> R.string.enroll_tick_why_reenroll
                        EnrollPlan.TickReason.LOOK_ALIKE -> R.string.enroll_tick_why_lookalike
                        EnrollPlan.TickReason.LINK -> R.string.enroll_tick_why_link
                    }
                    box.addView(label(getString(why), SP_S, color(R.color.text_muted)).apply { setPadding(dp(46), 0, dp(4), 0) })
                }
            }
        }

        // The button. "Already added" turns it into "Open <portal>".
        val blocker = p.blocker
        val button = addButton ?: return
        val open = blocker as? EnrollPlan.Blocker.AlreadyAdded
        val enabled = !busy && (open != null || (p.canAdd && deviceName.isNotBlank()))
        button.text = when {
            busy -> getString(R.string.enroll_adding)
            open != null -> getString(R.string.enroll_open_portal, open.portal.identity.nickname)
            reenroll -> getString(R.string.enroll_add_again)
            else -> getString(R.string.enroll_add)
        }
        button.isEnabled = enabled
        button.alpha = if (enabled) 1f else 0.4f
        button.contentDescription = button.text
    }

    /** The banners, from the strongest: a blocker, the finding, then the link. */
    private fun fillBanners(box: LinearLayout, p: EnrollPlan.Plan) {
        fun add(view: View) = box.addView(view, lp().margins(this, t = 8))
        val danger = color(R.color.danger)
        val dangerSoft = color(R.color.danger_soft)
        val warning = color(R.color.warning)
        val warningSoft = color(R.color.warning_soft)

        when (val b = p.blocker) {
            is EnrollPlan.Blocker.AlreadyAdded -> add(noteBox(R.drawable.ic_shield, danger, dangerSoft,
                getString(R.string.enroll_already_title, b.portal.identity.nickname), getString(R.string.enroll_already_body)))
            is EnrollPlan.Blocker.KeyOfOtherPortal -> add(noteBox(R.drawable.ic_shield, danger, dangerSoft,
                getString(R.string.enroll_key_other_title, b.portal.identity.nickname), getString(R.string.enroll_key_other_body)))
            EnrollPlan.Blocker.CapReached -> add(noteBox(R.drawable.ic_shield, danger, dangerSoft,
                getString(R.string.portal_cap, PortalAdmission.MAX_PORTALS), null))
            EnrollPlan.Blocker.LinkCannotReenroll -> add(noteBox(R.drawable.ic_shield, danger, dangerSoft, getString(R.string.enroll_link_no_reenroll), null))
            EnrollPlan.Blocker.BadCode -> add(noteBox(R.drawable.ic_shield, danger, dangerSoft, getString(R.string.enroll_bad_code), null))
            null -> {}
        }
        if (p.blocker == null || p.blocker is EnrollPlan.Blocker.CapReached) {
            when (val f = p.finding) {
                is LookAlike.Finding.SimilarAddress -> {
                    val both = android.text.SpannableStringBuilder(markedAddress(p.shownAddress, p.changedRanges))
                    p.otherAddress?.let { both.append("  ").append(getString(R.string.enroll_lookalike_against, it)) }
                    add(noteBox(R.drawable.ic_shield, danger, dangerSoft,
                        getString(R.string.enroll_lookalike_title, f.portal.identity.nickname), getString(R.string.enroll_lookalike_body),
                        extra = mono(both, SP_S, color(R.color.text), lines = 3)))
                }
                is LookAlike.Finding.SameAddressOtherKey -> add(noteBox(R.drawable.ic_shield, danger, dangerSoft,
                    getString(R.string.enroll_same_address_title, f.portal.identity.nickname), getString(R.string.enroll_same_address_body)))
                is LookAlike.Finding.SimilarName -> add(noteBox(R.drawable.ic_shield, danger, dangerSoft,
                    getString(R.string.enroll_lookalike_name_title, f.portal.identity.nickname), getString(R.string.enroll_lookalike_name_body)))
                else -> {}
            }
        }
        if (p.international) add(noteBox(R.drawable.ic_warning, warning, warningSoft, getString(R.string.enroll_international), null))
        if (p.fromLink && p.blocker != EnrollPlan.Blocker.LinkCannotReenroll) {
            add(noteBox(R.drawable.ic_warning, warning, warningSoft, getString(R.string.enroll_link_title), getString(R.string.enroll_link_body),
                extra = label(referrerLine(p.referrerText), SP_S, color(R.color.text_muted))))
        }
        if (target != null && p.blocker == null) {
            add(label(getString(if (p.keyChanged) R.string.enroll_key_differs else R.string.enroll_key_same), SP_S,
                if (p.keyChanged) danger else color(R.color.text_muted)).apply { setPadding(dp(4), 0, dp(4), 0) })
        }
    }

    /** A colored note with an icon, a bold line, an optional line and an optional extra view. */
    private fun noteBox(iconRes: Int, fg: Int, bg: Int, title: String, body: String?, extra: View? = null): LinearLayout =
        lin(vertical = false, padH = 12, padV = 10).apply {
            gravity = Gravity.TOP
            background = rounded(bg, dpf(CARD_RADIUS), fg and 0x4DFFFFFF, dp(1))
            addView(icon(iconRes, fg, 18), lp(dp(18), dp(18)).margins(this@AddServerActivity, r = 10, t = 1))
            val col = lin()
            col.addView(label(title, SP_M, color(R.color.text_strong), MEDIUM))
            body?.let { col.addView(label(it, SP_S, color(R.color.text)), lp().margins(this@AddServerActivity, t = 2)) }
            extra?.let { col.addView(it, lp().margins(this@AddServerActivity, t = 4)) }
            addView(col, lp(0, WRAP, 1f))
            accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
        }

    private fun nameProblem(c: PortalNaming.Check.Invalid): String = when (c.reason) {
        PortalNaming.Reason.EMPTY -> getString(R.string.name_err_empty)
        PortalNaming.Reason.TOO_LONG -> getString(R.string.name_err_long)
        PortalNaming.Reason.HIDDEN_CHARACTERS -> getString(R.string.name_err_hidden)
        PortalNaming.Reason.ONLY_SEPARATORS -> getString(R.string.name_err_separators)
        PortalNaming.Reason.TAKEN -> getString(R.string.name_err_taken, c.conflict?.nickname ?: "")
    }

    private fun rebuildPicker() {
        val box = pickerBox ?: return
        val p = computePlan() ?: return
        box.removeAllViews()
        box.addView(colorPicker(p.colors, p.colorIndex) { index -> colorChoice = index; rebuildPicker(); update() })
    }

    // ------------------------------------------------------------ the enrollment

    private fun submit() {
        if (busy) return
        val p = computePlan() ?: return
        (p.blocker as? EnrollPlan.Blocker.AlreadyAdded)?.let { return openPortal(it.portal.identity.id) }
        val i = info ?: return
        val text = codeText ?: return
        val device = deviceName.trim()
        if (!p.canAdd || device.isEmpty()) return update()
        plan = p
        val reenroll = target
        val nick = (p.nickname as? PortalNaming.Check.Valid)?.nickname
        val color = p.colorIndex
        statusText?.visibility = View.GONE
        setBusy(true)
        Thread {
            var serverId = -1L
            val error: EnrollmentError? = try {
                val e = Enroller.enroll(this, text, device)
                // The key of the code is the pin: the stored key is the one of the code.
                if (!e.serverPublicKey.contentEquals(i.serverPublicKey)) throw EnrollmentError(getString(R.string.err_server_key))
                val sealed = KeyVault.seal(e.devicePrivateKey)
                e.devicePrivateKey.fill(0)
                val store = Store.get(this)
                if (reenroll != null) {
                    val ok = store.reenroll(reenroll.identity.id, e.address.http, e.serverName, e.deviceId, sealed, e.serverPublicKey, e.userName)
                    if (!ok) throw EnrollmentError(getString(R.string.portal_add_conflict))
                    serverId = reenroll.identity.id
                } else {
                    val added = store.addServer(
                        Store.NewPortal(e.address.http, e.serverName, e.deviceId, device, sealed, e.serverPublicKey, e.userName, nick ?: "", color)
                    )
                    when (added) {
                        is Store.AddResult.Added -> serverId = added.id
                        is Store.AddResult.Refused -> throw EnrollmentError(refusalText(added.refusal))
                        Store.AddResult.Duplicate -> throw EnrollmentError(getString(R.string.portal_add_conflict))
                    }
                }
                null
            } catch (e: EnrollmentError) {
                e
            } catch (e: Exception) {
                EnrollmentError(getString(R.string.add_error, e.message ?: e.javaClass.simpleName))
            }
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                setBusy(false)
                if (error == null) done(serverId, reenroll != null) else fail(error)
            }
        }.start()
    }

    private fun setBusy(on: Boolean) {
        busy = on
        update()
    }

    private fun refusalText(r: PortalAdmission.Refusal): String = when (r) {
        is PortalAdmission.Refusal.DuplicateKey -> getString(R.string.portal_duplicate, r.portal.identity.nickname)
        PortalAdmission.Refusal.CapReached -> getString(R.string.portal_cap, PortalAdmission.MAX_PORTALS)
        else -> getString(R.string.portal_add_conflict)
    }

    private fun fail(e: EnrollmentError) {
        val text = e.message ?: getString(R.string.err_unknown)
        // An error about the code returns to the first step, where the code can be fixed.
        if (e.field == app.portalinbox.net.EnrollField.CODE) {
            codeError = text
            info = null
            showCodeStep()
            return
        }
        statusText?.apply {
            this.text = text
            visibility = View.VISIBLE
        }
    }

    private fun openPortal(id: Long) {
        startActivities(arrayOf(
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            Intent(this, PortalActivity::class.java).putExtra(PortalActivity.EXTRA_SERVER, id),
        ))
        finish()
    }

    /** Enrolled. The code has no use any more. A new portal opens its screen (the groups). A portal enrolled again returns to its screen. */
    private fun done(serverId: Long, again: Boolean) {
        codeText = null
        codeDraft = ""
        if (again) {
            // The old session used the old key and the old device: the connection starts again with the new values.
            Registry.connections.remove(serverId)?.stop()
            InboxService.start(this)
            finish()
        } else {
            InboxService.start(this)
            openPortal(serverId)
        }
    }

    /** One line that names a portal: the monogram, the nickname and the host. */
    private fun identityLine(portal: KnownPortal): LinearLayout = lin(vertical = false).apply {
        addView(monogramSquare(portal.identity, 28), lp(dp(28), dp(28)).margins(this@AddServerActivity, r = 10))
        addView(label(portal.identity.nickname, SP_L, portalColor(portal.identity), BOLD, lines = 1), lp(WRAP, WRAP))
        addView(label("  ·  " + app.portalinbox.portal.PortalRow.hostOf(portal.address), SP_S, color(R.color.text_muted), lines = 1), lp(0, WRAP, 1f))
    }

    private fun simpleWatcher(onText: (String) -> Unit) = object : android.text.TextWatcher {
        override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
        override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
        override fun afterTextChanged(s: android.text.Editable?) { onText(s?.toString().orEmpty()) }
    }
}

/**
 * The enrollment again of one portal (proposal 3.6). It is not exported: only the screen of a portal on this phone opens it, with the
 * id of the portal. A link from another app can never reach it, so a link can never replace the key of a portal in place.
 */
class ReenrollActivity : AddServerActivity() {
    companion object {
        const val EXTRA_PORTAL = "portal_id"
    }

    override fun reenrollId(): Long? = intent.getLongExtra(EXTRA_PORTAL, -1L).takeIf { it >= 0 }
}
