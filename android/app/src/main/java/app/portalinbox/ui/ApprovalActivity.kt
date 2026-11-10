package app.portalinbox.ui

import android.content.Context
import android.content.Intent
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.InputFilter
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.ScrollView
import android.widget.TextView
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import app.portalinbox.R
import app.portalinbox.alert.ItemAlerter
import app.portalinbox.core.NativeRender
import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Item
import app.portalinbox.data.ItemRepository
import app.portalinbox.data.OutboxAnswer
import app.portalinbox.data.Server
import app.portalinbox.data.ShownEffect
import app.portalinbox.data.Store
import app.portalinbox.data.Submit
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PortalLabels
import app.portalinbox.proto.Language
import app.portalinbox.render.CodeSpans
import app.portalinbox.service.Items
import app.portalinbox.service.Registry
import app.portalinbox.service.ServerClock
import java.time.ZoneId

/**
 * The approval screen. It only draws. The decisions are in pure classes: [ApprovalState] (which buttons work), [ReadGate]
 * (the command was read to the end), [VisibleText] (hidden characters), [ExplanationText], [BiometricPolicy], [ItemTexts].
 *
 * Order (CONF 5.3, mock of checkpoint C3): the header, the zone COMMAND (the text of the service), the zone AGENT EXPLANATION
 * (the text of a third party, other background and dashed edge), the context lines, the secondary buttons. Decline and Approve
 * sit in a fixed bottom bar. Approve stays locked until the command was read to the end, and then asks for the local biometric.
 */
class ApprovalActivity : BaseFragmentActivity() {
    companion object {
        private const val EXTRA_SERVER = "server_id"
        private const val EXTRA_REQUEST = "request_id"

        fun intent(context: Context, serverId: Long, requestId: String): Intent =
            Intent(context, ApprovalActivity::class.java)
                .setAction("app.portalinbox.OPEN_APPROVAL:$serverId:$requestId")
                .putExtra(EXTRA_SERVER, serverId)
                .putExtra(EXTRA_REQUEST, requestId)
    }

    /** Lot A3 sets this to open the discussion screen. While it is null, the button "Ask the agent" does not show. */
    object Nav {
        @Volatile var ask: ((Context, Long, String) -> Unit)? = null
    }

    private var serverId = -1L
    private var requestId = ""
    private lateinit var repo: ItemRepository
    private val handler = Handler(Looper.getMainLooper())
    private val onChange: () -> Unit = { refresh() }
    private val tick = object : Runnable {
        override fun run() {
            refresh()
            handler.postDelayed(this, 30_000)
        }
    }

    // state of the screen
    private var gate = ReadGate.start("")
    private var savedGateKey: String? = null
    private var savedGateRead = false
    private var staleConfirmed = false
    private val explanationZone by lazy { ExplanationZone(this, { snackbar(it) }) }
    private var commentOpen = false
    private var commentText = ""
    private var proposeChange = false
    private var shownKey: String? = null
    private var promptEffect: String? = null
    private var item: Item? = null

    /** The portal of the item. The banner, the footer, the prompt and the clipboard label name it: the portal never names itself. */
    private var server: Server? = null

    // views
    private lateinit var scroller: ScrollView
    private lateinit var content: LinearLayout
    private lateinit var codeLabel: TextView
    private lateinit var hintView: TextView
    private lateinit var linkView: TextView
    private lateinit var declineButton: TextView
    private lateinit var approveButton: TextView
    private lateinit var prompt: BiometricPrompt

    // parts that the refresh updates
    private var headerLine: TextView? = null
    private var notice: NoticeBanner? = null
    private var gateRow: LinearLayout? = null
    private var gateText: TextView? = null
    private var gatePercent: TextView? = null
    private var gateBar: ProgressBar? = null
    private var gateIcon: android.widget.ImageView? = null
    private var contextBox: LinearLayout? = null
    private var staleBox: LinearLayout? = null
    private var staleCheck: CheckBox? = null
    private var askButton: View? = null
    private var commentToggle: View? = null
    private var commentForm: LinearLayout? = null

    /** The box "From agent" (lot G4). It changes when the owner allows the agent, with no new request: it has its own holder. */
    private var agentHolder: LinearLayout? = null
    private var agentBoxKey: String? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        requestId = intent.getStringExtra(EXTRA_REQUEST) ?: ""
        repo = Items.repository(this)
        server = Store.get(this).server(serverId)
        savedInstanceState?.let {
            savedGateKey = it.getString("gate_key")
            savedGateRead = it.getBoolean("gate_read")
            staleConfirmed = it.getBoolean("stale_confirmed")
            explanationZone.expanded = it.getBoolean("expanded")
            explanationZone.rawMarkdown = it.getBoolean("raw_markdown")
            commentOpen = it.getBoolean("comment_open")
            commentText = it.getString("comment_text", "")
            proposeChange = it.getBoolean("propose")
        }

        val main = lin()
        codeLabel = mono("", SP_S, color(R.color.text_muted), bold = true)
        main.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }, codeLabel))
        content = lin(padH = 10, padV = 4)
        scroller = scroll(content)
        main.addView(scroller, lp(MATCH, 0, 1f))
        server?.let { main.addView(PortalBanner.footer(this, it)) }
        main.addView(bottomBar())
        setScreen(main)

        prompt = BiometricPrompt(this, ContextCompat.getMainExecutor(this), object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = onAuthenticated()

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                val quiet = errorCode == BiometricPrompt.ERROR_USER_CANCELED || errorCode == BiometricPrompt.ERROR_NEGATIVE_BUTTON ||
                    errorCode == BiometricPrompt.ERROR_CANCELED
                if (!quiet) snackbar(errString.toString())
            }
        })
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString("gate_key", gate.key)
        outState.putBoolean("gate_read", gate.readToEnd)
        outState.putBoolean("stale_confirmed", staleConfirmed)
        outState.putBoolean("expanded", explanationZone.expanded)
        outState.putBoolean("raw_markdown", explanationZone.rawMarkdown)
        outState.putBoolean("comment_open", commentOpen)
        outState.putString("comment_text", commentForm?.findViewWithTag<EditText>("comment")?.text?.toString() ?: commentText)
        outState.putBoolean("propose", proposeChange)
    }

    override fun onResume() {
        super.onResume()
        Registry.addListener(onChange)
        handler.post(tick)
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
        handler.removeCallbacks(tick)
        commentText = commentForm?.findViewWithTag<EditText>("comment")?.text?.toString() ?: commentText
    }

    private var leaving = false

    /** Back from a notification: the task holds only this screen, so Back goes to the home list, not out of the app. */
    override fun finish() {
        if (isTaskRoot && !leaving) {
            leaving = true
            startActivity(MainActivity.home(this))
        }
        super.finish()
    }

    // ------------------------------------------------------------ bottom bar

    private fun bottomBar(): View {
        val bar = lin(padH = 10, padV = 8)
        bar.addView(divider(), lp(MATCH, 1).margins(this, b = 6))
        linkView = label("", SP_S, color(R.color.warning), MEDIUM).apply { gravity = Gravity.CENTER; visibility = View.GONE }
        bar.addView(linkView, lp().margins(this, b = 4))
        hintView = label("", SP_S, color(R.color.text_muted)).apply { gravity = Gravity.CENTER }
        bar.addView(hintView, lp().margins(this, b = 8))
        val row = lin(vertical = false)
        declineButton = button(getString(R.string.approval_decline), ButtonKind.SECONDARY) { onDecline() }
        approveButton = button(getString(R.string.approval_approve), ButtonKind.PRIMARY) { onApprove() }
        row.addView(declineButton, lp(0, WRAP, 1f).margins(this, r = 6))
        row.addView(approveButton, lp(0, WRAP, 1.2f).margins(this, l = 6))
        bar.addView(row)
        bar.setBackgroundColor(color(R.color.bg_page))
        return bar
    }

    // ------------------------------------------------------------ refresh

    private fun now(): Long = ServerClock.now(this, serverId)

    private fun gateKeyOf(i: Item): String = i.computedEffect ?: i.requestId

    private fun refresh() {
        val current = repo.item(serverId, requestId)
        if (current == null) {
            showGone()
            return
        }
        item = current
        val key = "${current.request.seq}|${current.computedEffect}|${current.validity}|${current.request.approval?.explanation}"
        if (key != shownKey) {
            if (gate.key != gateKeyOf(current)) {
                // A command that the owner read before a turn of the screen stays read. A new command starts locked.
                gate = ReadGate.restore(gateKeyOf(current), savedGateKey, savedGateRead)
                savedGateKey = null
                savedGateRead = false
            }
            buildContent(current)
            shownKey = key
        }
        refreshDynamic(current)
    }

    private fun showGone() {
        content.removeAllViews()
        content.addView(label(getString(R.string.approval_gone), SP_M, color(R.color.text_muted)), lp().margins(this, t = 24))
        hintView.text = ""
        declineButton.visibility = View.GONE
        approveButton.visibility = View.GONE
    }

    private fun screenOf(i: Item): ApprovalState.Screen {
        val progress = OutboxAnswer.progress(repo.outboxOf(serverId, requestId))
        return ApprovalState.of(
            ApprovalState.Input(
                item = i, nowMs = now(), gate = gate, staleConfirmed = staleConfirmed, answer = progress,
                canAuthenticate = canAuthenticate(), askAvailable = Nav.ask != null,
                link = ApprovalState.linkOf(Registry.status(serverId).state), portal = server?.nickname ?: "this portal",
                agentLocked = agentBox(i).locked,
            ),
        )
    }

    private fun agentDirectory() = AgentDirectory(repo.agents(serverId))

    private fun agentBox(i: Item): FromAgentBox.Box = FromAgentBox.of(i, agentDirectory())

    /** Draws the box "From agent" again when what it says changed: the owner allowed the agent, renamed it, or the agent says another name. */
    private fun refreshAgentBox(i: Item) {
        val holder = agentHolder ?: return
        val b = agentBox(i)
        val key = listOf(b.name, b.stateWord, b.keyName, b.says, b.saysHere, b.locked, b.warnings, b.canOpenAgent).joinToString("|")
        if (key == agentBoxKey) return
        agentBoxKey = key
        holder.removeAllViews()
        holder.addView(fromAgentBox(b) { b.agent?.let { startActivity(AgentActivity.intent(this, serverId, it.agentId)) } })
    }

    private fun refreshDynamic(i: Item) {
        refreshAgentBox(i)
        val s = screenOf(i)
        val progress = OutboxAnswer.progress(repo.outboxOf(serverId, requestId))
        val now = now()
        codeLabel.text = i.request.approval?.code ?: ""
        headerLine?.text = ItemTexts.headerLine(i, now)

        // The strip at the top: an invalid request, how a closed item ended, the result of an answer.
        val result = (progress as? AnswerProgress.Done)?.let { OutboxAnswer.resultLine(it) }
        val closedByOwner = s.mode == ApprovalState.Mode.CLOSED && i.settled == null && result != null
        val message = when {
            closedByOwner -> result
            s.notice != null && result != null && s.mode != ApprovalState.Mode.CLOSED -> s.notice + "\n" + result
            s.notice != null -> s.notice
            else -> if ((progress as? AnswerProgress.Done)?.let { OutboxAnswer.canTryAgain(it) } == true) result else null
        }
        val danger = s.mode == ApprovalState.Mode.INVALID
        notice?.show(
            message,
            if (danger) color(R.color.danger) else if (s.mode == ApprovalState.Mode.CLOSED) color(R.color.text_muted) else color(R.color.warning),
            if (danger) color(R.color.danger_soft) else if (s.mode == ApprovalState.Mode.CLOSED) color(R.color.bg_hover) else color(R.color.warning_soft),
        )

        // The gate line under the command.
        val read = gate.readToEnd
        gateIcon?.setImageResource(if (read) R.drawable.ic_check else R.drawable.ic_lock)
        gateIcon?.imageTintList = ColorStateList.valueOf(if (read) color(R.color.primary) else color(R.color.text_muted))
        gateText?.text = getString(if (read) R.string.approval_read_done else R.string.approval_read_to_unlock)
        gateText?.setTextColor(if (read) color(R.color.primary) else color(R.color.text_muted))
        gateBar?.visibility = if (read) View.GONE else View.VISIBLE
        gateBar?.progress = gate.progressPercent
        gatePercent?.visibility = if (read) View.GONE else View.VISIBLE
        gatePercent?.text = "${gate.progressPercent}%"
        gateRow?.visibility = if (s.mode == ApprovalState.Mode.CLOSED || s.mode == ApprovalState.Mode.INVALID) View.GONE else View.VISIBLE

        // Context lines and the stale box.
        contextBox?.let { box ->
            box.removeAllViews()
            for (line in ItemTexts.contextLines(i, now)) box.addView(label(line, SP_S, color(R.color.text_muted)), lp().margins(this, t = 2))
            box.visibility = if (box.childCount == 0) View.GONE else View.VISIBLE
        }
        staleBox?.visibility = if (s.staleBoxShown) View.VISIBLE else View.GONE
        staleCheck?.let { if (s.staleBoxText != null) it.text = s.staleBoxText }
        askButton?.visibility = if (s.askShown) View.VISIBLE else View.GONE
        // With a discussion going on, the button opens it: the owner sees the turns and the state of the question.
        (askButton as? TextView)?.text = getString(
            if (i.state == app.portalinbox.proto.ItemState.REPLIED || repo.turns(serverId, requestId).isNotEmpty()) R.string.approval_discussion else R.string.approval_ask,
        )
        commentToggle?.visibility = if (s.declineWithComment) View.VISIBLE else View.GONE
        commentForm?.visibility = if (s.declineWithComment && commentOpen) View.VISIBLE else View.GONE

        // The bottom bar.
        hintView.text = s.hint ?: ""
        hintView.visibility = if (s.hint == null) View.GONE else View.VISIBLE
        linkView.text = s.linkLine ?: ""
        linkView.visibility = if (s.linkLine == null) View.GONE else View.VISIBLE
        linkView.setTextColor(color(if (s.approveLock == ApprovalState.Lock.PORTAL_BLOCKED) R.color.danger else R.color.warning))
        val bottom = s.mode != ApprovalState.Mode.CLOSED
        declineButton.visibility = if (bottom) View.VISIBLE else View.GONE
        approveButton.visibility = if (s.approveShown) View.VISIBLE else View.GONE
        declineButton.alpha = if (s.declineEnabled) 1f else 0.45f
        val on = s.approveEnabled
        approveButton.background = if (on) pressable(color(R.color.primary), dpf(CONTROL_RADIUS))
        else pressable(color(R.color.bg_hover), dpf(CONTROL_RADIUS), color(R.color.hairline))
        approveButton.setTextColor(if (on) color(R.color.on_primary) else color(R.color.text_subtle))
        val iconRes = if (on) R.drawable.ic_check else R.drawable.ic_lock
        approveButton.setCompoundDrawablesRelativeWithIntrinsicBounds(
            getDrawable(iconRes)?.mutate()?.apply {
                setTint(if (on) color(R.color.on_primary) else color(R.color.text_subtle))
                setBounds(0, 0, dp(18), dp(18))
            },
            null, null, null,
        )
        approveButton.compoundDrawablePadding = dp(8)
        approveButton.text = approveText(s)
        val spoken = getString(if (s.queued) R.string.approval_queue else R.string.approval_approve) + " " + onPortal()
        approveButton.contentDescription = if (on) spoken else spoken + ". " + (s.hint ?: "")
    }

    /** "on Production": the portal that the answer goes to. */
    private fun onPortal(): String = server?.let { getString(R.string.approval_on_portal, PortalIdentity.of(it).nickname) } ?: ""

    /** The label of Approve: "Approve" (or "Queue approval" while the portal is offline), and on a second, smaller line "on Production". */
    private fun approveText(s: ApprovalState.Screen): CharSequence {
        val first = getString(if (s.queued) R.string.approval_queue else R.string.approval_approve)
        val second = onPortal()
        if (second.isEmpty()) return first
        return android.text.SpannableStringBuilder(first).append('\n').append(
            android.text.SpannableString(second).apply {
                setSpan(android.text.style.RelativeSizeSpan(0.8f), 0, length, android.text.Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            },
        )
    }

    // ------------------------------------------------------------ content

    private fun buildContent(i: Item) {
        val approval = i.request.approval
        content.removeAllViews()
        headerLine = null; notice = null; gateRow = null; gateText = null; gatePercent = null; gateBar = null; gateIcon = null
        contextBox = null; staleBox = null; staleCheck = null; askButton = null; commentToggle = null; commentForm = null
        agentHolder = null
        val now = now()
        val zone = ZoneId.systemDefault()

        // The identity of the portal comes first, above the title, for every state of the item.
        server?.let { content.addView(PortalBanner.banner(this, it), lp().margins(this, l = -10, r = -10, t = 4)) }
        content.addView(label(ItemTexts.title(i), SP_XL, color(R.color.text_strong), MEDIUM).apply {
            if (Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
        }, lp().margins(this, t = 8, l = 4, r = 4))
        content.addView(label(ItemTexts.senderLine(i, agentDirectory().labelOf(i)), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4, l = 4, r = 4))
        // The box "From agent": the name, the state and the key are the portal's words; what the agent says is marked "Not verified".
        agentHolder = lin()
        agentBoxKey = null
        content.addView(agentHolder, lp().margins(this, t = 8))
        refreshAgentBox(i)
        val chips = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        chips.addView(chip(getString(R.string.chip_approval), color(R.color.primary), color(R.color.primary_soft)))
        headerLine = label(ItemTexts.headerLine(i, now), SP_S, color(R.color.text_muted))
        chips.addView(headerLine, lp(0, WRAP, 1f).margins(this, l = 8))
        content.addView(chips, lp().margins(this, t = 8, l = 4, r = 4))
        ItemTexts.leaseLine(i, zone)?.let {
            content.addView(label(it, SP_S, color(R.color.text_subtle)), lp().margins(this, t = 4, l = 4, r = 4))
        }

        notice = NoticeBanner(this)
        content.addView(notice, lp().margins(this, t = 10))
        if (approval == null || !i.validity.isValid || i.view == null) return

        val view = i.view
        val language = approval.language?.takeIf { it != Language.PLAIN }
        // The command: the colors of the hint, then the marks of the hidden characters over the colors.
        val colored = CodeSpans.build(language?.wire, view.body, NativeRender)
        val marked = VisibleText.render(colored, nonAscii = true)
        val fieldMarks = view.fields.map { VisibleText.render(it.value, nonAscii = true) }
        val report = VisibleText.combine(listOf(marked) + fieldMarks)
        VisibleText.banner(report)?.let { text ->
            val alarm = VisibleText.bannerIsAlarm(report)
            val b = NoticeBanner(this)
            b.show(text, color(if (alarm) R.color.danger else R.color.warning), color(if (alarm) R.color.danger_soft else R.color.warning_soft))
            content.addView(b, lp().margins(this, t = 10))
        }

        content.addView(commandCard(i, marked, fieldMarks, language, report), lp().margins(this, t = 10))
        (explanationZone.view.parent as? android.view.ViewGroup)?.removeView(explanationZone.view)
        content.addView(explanationZone.view, lp().margins(this, t = 10))
        explanationZone.show(approval.explanation, approval.explanationFormat)

        contextBox = lin(padH = 4)
        content.addView(contextBox, lp().margins(this, t = 10))

        staleBox = lin(vertical = false, padH = 12, padV = 4).apply {
            background = rounded(color(R.color.warning_soft), dpf(CARD_RADIUS), color(R.color.warning) and 0x66FFFFFF, dp(1))
            visibility = View.GONE
        }
        staleCheck = CheckBox(this).apply {
            textSize = SP_M
            minHeight = dp(48)
            setTextColor(color(R.color.text))
            buttonTintList = ColorStateList.valueOf(color(R.color.warning))
            isChecked = staleConfirmed
            setOnCheckedChangeListener { _, checked ->
                staleConfirmed = checked
                item?.let { refreshDynamic(it) }
            }
        }
        staleBox!!.addView(staleCheck, lp(0, WRAP, 1f))
        content.addView(staleBox, lp().margins(this, t = 10))

        askButton = button(getString(R.string.approval_ask), ButtonKind.SECONDARY) { Nav.ask?.invoke(this, serverId, requestId) }
            .apply {
                setCompoundDrawablesRelativeWithIntrinsicBounds(
                    getDrawable(R.drawable.ic_help)?.mutate()?.apply { setTint(color(R.color.text)); setBounds(0, 0, dp(18), dp(18)) }, null, null, null,
                )
                compoundDrawablePadding = dp(8)
            }
        content.addView(askButton, lp().margins(this, t = 10))

        commentToggle = button(getString(R.string.approval_decline_comment), ButtonKind.DANGER) {
            commentOpen = !commentOpen
            item?.let { refreshDynamic(it) }
        }
        content.addView(commentToggle, lp().margins(this, t = 10))
        commentForm = commentForm(i)
        content.addView(commentForm, lp().margins(this, t = 8, b = 16))
    }

    private fun commandCard(
        i: Item, marked: VisibleText.Marked, fieldMarks: List<VisibleText.Marked>, language: Language?, report: VisibleText.Marked,
    ): View {
        val view = i.view!!
        val approval = i.request.approval!!
        val card = lin().apply {
            background = solidCard()
            clipToOutline = true
            outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
        }
        val head = lin(vertical = false, padH = 12, padV = 8).apply { gravity = Gravity.CENTER_VERTICAL }
        head.addView(caption(getString(R.string.approval_command)))
        head.addView(caption(getString(R.string.approval_from_service), color(R.color.primary)), lp(0, WRAP, 1f).margins(this, l = 8))
        head.addView(copyButton(getString(R.string.approval_copy_description)) {
            // The command goes to the clipboard as it came, byte for byte: never the marked text.
            copyToClipboard(server?.let { PortalLabels.clipboardLabel(PortalIdentity.of(it)) } ?: "command", view.body)
            snackbar(getString(R.string.approval_copied))
        })
        card.addView(head)
        card.addView(divider())

        // The fields of the view, then the short effect.
        val facts = ArrayList<Pair<String, CharSequence>>()
        view.fields.forEachIndexed { n, f -> facts += f.label to SpanApplier(this, CodePalette.COMMAND).apply(fieldMarks[n].text) }
        ShownEffect.short(i.computedEffect ?: "")?.let { facts += getString(R.string.approval_effect) to "sha256:$it" }
        val grid = lin(padH = 12, padV = 8)
        facts.chunked(2).forEach { pair ->
            val r = lin(vertical = false).apply { gravity = Gravity.TOP }
            pair.forEach { (k, v) ->
                val cell = lin()
                cell.addView(caption(k, color(R.color.text_muted)))
                cell.addView(mono(v, SP_M, color(R.color.text), lines = 3).apply { setTextIsSelectable(false) }, lp().margins(this, t = 1))
                r.addView(cell, lp(0, WRAP, 1f).margins(this, r = 8))
            }
            if (pair.size == 1) r.addView(View(this), lp(0, 1, 1f))
            grid.addView(r, lp().margins(this, b = 6))
        }
        if (facts.isNotEmpty()) {
            card.addView(grid)
            card.addView(divider())
        }
        view.bodyLabel?.let {
            card.addView(caption(VisibleText.escapeLine(it), color(R.color.text_muted)).apply { setPadding(dp(12), dp(8), dp(12), dp(4)) })
        }

        // The command: a scroll box. The gate reads its scroll.
        val applier = SpanApplier(this, CodePalette.COMMAND)
        val sb = applier.apply(marked.text)
        val textSize = 13f
        val text = mono(sb, textSize, color(R.color.text), lines = Int.MAX_VALUE).apply {
            maxLines = Int.MAX_VALUE
            ellipsize = null
            setTextIsSelectable(false)
            textDirection = View.TEXT_DIRECTION_LTR
            setLineSpacing(dpf(2f), 1f)
            setPadding(dp(8), dp(8), dp(12), dp(4))
        }
        applier.numberLines(sb, text.textSize)
        text.text = sb
        val inner = lin().apply { setBackgroundColor(color(R.color.code_bg)) }
        inner.addView(text, lp())
        inner.addView(mono(getString(R.string.approval_end_of_text), SP_S, color(R.color.text_subtle)).apply {
            typeface = Typeface.create(Typeface.MONOSPACE, Typeface.ITALIC)
            gravity = Gravity.END
            setPadding(dp(8), dp(2), dp(12), dp(8))
        }, lp())
        val key = gate.key
        val box = GateScrollView(this, dp(9 * 20 + 20)) { y, viewport, total ->
            val before = gate
            gate = ReadGate.observe(gate, key, y, viewport, total)
            if (gate != before) item?.let { refreshDynamic(it) }
        }
        box.addView(inner, android.view.ViewGroup.LayoutParams(MATCH, WRAP))
        box.contentDescription = getString(R.string.approval_command) + ". " + view.body
        card.addView(box, lp())
        card.addView(divider())

        // The gate line.
        gateRow = lin(vertical = false, padH = 12, padV = 6).apply { gravity = Gravity.CENTER_VERTICAL }
        gateIcon = icon(R.drawable.ic_lock, color(R.color.text_muted), 16)
        gateRow!!.addView(gateIcon, lp(dp(16), dp(16)).margins(this, r = 8))
        gateText = label("", SP_S, color(R.color.text_muted))
        gateRow!!.addView(gateText, lp(WRAP, WRAP))
        gateBar = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
            max = 100
            progressTintList = ColorStateList.valueOf(color(R.color.primary))
            progressBackgroundTintList = ColorStateList.valueOf(color(R.color.hairline))
            progressBackgroundTintMode = android.graphics.PorterDuff.Mode.SRC_IN
        }
        gateRow!!.addView(gateBar, lp(0, dp(4), 1f).margins(this, l = 10, r = 10))
        gatePercent = mono("0%", SP_S, color(R.color.text_muted))
        gateRow!!.addView(gatePercent)
        card.addView(gateRow)
        card.addView(divider())

        // The foot: the numbers of the command and the marks.
        val sqlLike = language == null || language == Language.SQL
        val words = VisibleText.facts(view.body, marked, sqlLike).words().toMutableList()
        if (language != null) words += "colors: ${language.wire} hint, text unchanged"
        val foot = lin(padH = 12, padV = 8)
        foot.addView(mono(words.joinToString("  "), SP_S, color(R.color.text_subtle), lines = 4))
        if (report.hasMarks) foot.addView(mono("${report.markCount} marked", SP_S, color(R.color.danger), bold = true))
        card.addView(foot)
        return card
    }

    // ------------------------------------------------------------ the zone of the agent

    // ------------------------------------------------------------ the comment form

    private fun commentForm(i: Item): LinearLayout {
        val form = lin(padH = 12, padV = 10).apply {
            background = solidCard()
            visibility = View.GONE
        }
        val field = EditText(this).apply {
            tag = "comment"
            hint = getString(R.string.approval_comment_hint)
            setText(commentText)
            textSize = SP_M
            minLines = 3
            gravity = Gravity.TOP
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            // The field stops at the limit: nothing is cut afterwards.
            filters = arrayOf(InputFilter.LengthFilter(ApprovalState.MAX_DECLINE_CHARS))
            setTextColor(color(R.color.text))
            setHintTextColor(color(R.color.text_subtle))
            background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
            setPadding(dp(12), dp(10), dp(12), dp(10))
        }
        form.addView(field, lp())
        val propose = CheckBox(this).apply {
            text = getString(R.string.approval_propose_change)
            textSize = SP_M
            minHeight = dp(48)
            isChecked = proposeChange
            setTextColor(color(R.color.text))
            buttonTintList = ColorStateList.valueOf(color(R.color.primary))
            setOnCheckedChangeListener { _, checked -> proposeChange = checked }
        }
        form.addView(propose, lp().margins(this, t = 4))
        form.addView(label(getString(R.string.approval_cannot_edit), SP_S, color(R.color.text_muted)), lp())
        form.addView(button(getString(R.string.approval_send_decline), ButtonKind.DANGER) {
            val text = field.text.toString()
            when (val d = ApprovalState.decline(text, propose.isChecked, i.request.allowCustom)) {
                is ApprovalState.Decline.Refused -> snackbar(d.reason)
                ApprovalState.Decline.Plain -> submitDecline(null, null)
                is ApprovalState.Decline.WithText -> submitDecline(d.text, d.intent)
            }
        }, lp().margins(this, t = 8))
        return form
    }

    // ------------------------------------------------------------ the answers

    private fun onDecline() {
        val i = item ?: return
        if (!screenOf(i).declineEnabled) return
        submitDecline(null, null)
    }

    private fun submitDecline(text: String?, intent: app.portalinbox.proto.Intent?) {
        val i = item ?: return
        val r = Items.engine(this).decline(serverId, requestId, text, intent)
        afterSubmit(i, r)
    }

    private fun afterSubmit(i: Item, r: Submit) {
        OutboxAnswer.submitLine(r)?.let { snackbar(it) }
        if (r is Submit.Queued) {
            Registry.connections[serverId]?.flushOutbox()
            ItemAlerter.withdrawNow(this, i)
            commentOpen = false
            commentText = ""
        }
        Registry.changed()
        refresh()
    }

    private fun onApprove() {
        val i = item ?: return
        val s = screenOf(i)
        if (!s.approveEnabled) {
            s.hint?.let { snackbar(it) }
            return
        }
        if (!canAuthenticate()) {
            snackbar(getString(R.string.approval_no_lock))
            refresh()
            return
        }
        // The owner confirms for this command: the effect that is on the screen now.
        promptEffect = i.computedEffect
        val approval = i.request.approval ?: return
        val info = BiometricPrompt.PromptInfo.Builder()
            .setTitle(BiometricPolicy.title(server?.nickname ?: "this portal"))
            .setSubtitle(BiometricPolicy.subtitle(approval.code, approval.service, ShownEffect.short(i.computedEffect ?: "")))
            .setAllowedAuthenticators(authenticators())
            .build()
        prompt.authenticate(info)
    }

    /** The local gate said yes. The answer goes out only if the command is still the one that the owner read. */
    private fun onAuthenticated() {
        val i = repo.item(serverId, requestId)
        if (i == null || !i.canApprove || i.computedEffect != promptEffect || gate.key != gateKeyOf(i)) {
            snackbar(getString(R.string.approval_changed))
            refresh()
            return
        }
        afterSubmit(i, Items.engine(this).approve(serverId, requestId))
    }

    private fun authenticators(): Int = when (BiometricPolicy.allowed(Build.VERSION.SDK_INT)) {
        BiometricPolicy.Allowed.STRONG_OR_CREDENTIAL ->
            BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL
        BiometricPolicy.Allowed.WEAK_OR_CREDENTIAL ->
            BiometricManager.Authenticators.BIOMETRIC_WEAK or BiometricManager.Authenticators.DEVICE_CREDENTIAL
    }

    private fun canAuthenticate(): Boolean =
        BiometricManager.from(this).canAuthenticate(authenticators()) == BiometricManager.BIOMETRIC_SUCCESS
}
