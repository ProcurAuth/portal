package app.portalinbox.ui

import android.content.Context
import android.content.Intent
import android.content.res.ColorStateList
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.InputFilter
import android.text.InputType
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextWatcher
import android.text.style.ForegroundColorSpan
import android.text.style.RelativeSizeSpan
import android.view.Gravity
import android.view.View
import android.widget.CheckBox
import android.widget.CompoundButton
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.RadioButton
import android.widget.ScrollView
import android.widget.TextView
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import app.portalinbox.R
import app.portalinbox.alert.ItemAlerter
import app.portalinbox.core.NativeRender
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.AnswerProgress
import app.portalinbox.data.AnswerSheet
import app.portalinbox.data.BlockKind
import app.portalinbox.data.BlockStatus
import app.portalinbox.data.Item
import app.portalinbox.data.ItemRepository
import app.portalinbox.data.OutboxAnswer
import app.portalinbox.data.Server
import app.portalinbox.data.Sheet
import app.portalinbox.data.SheetBlock
import app.portalinbox.data.SheetDraft
import app.portalinbox.data.Store
import app.portalinbox.data.Submit
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PortalLabels
import app.portalinbox.proto.InputOption
import app.portalinbox.proto.ItemState
import app.portalinbox.proto.Language
import app.portalinbox.render.CodeSpans
import app.portalinbox.service.Items
import app.portalinbox.service.Registry
import app.portalinbox.service.ServerClock
import java.time.ZoneId
import java.util.concurrent.ConcurrentHashMap

/**
 * The screen of a question, of a decision of an automatic system, and of a form of 1 to 4 questions. It only draws. The decisions are in pure classes: [QuestionState] (which rows and buttons), [FlagsMatrix]
 * through it (the rows "Other answer" and "Ask the agent"), [AnswerSheet] (the draft, the rules of the server and the `answer` frame),
 * [QuestionHistory] (how a question ended), [BiometricPolicy], [TalkBackLabels].
 *
 * Order: the portal, the title, the box "From agent", the command (exact text, Copy), the explanation of the agent (not checked),
 * the suggestion, the question blocks, the row "Ask the agent". Decline to answer and Send sit in a fixed bottom bar. Send asks
 * for the local biometric. Decline to answer asks for nothing: it runs nothing.
 */
class QuestionActivity : BaseFragmentActivity() {
    companion object {
        private const val EXTRA_SERVER = "server_id"
        private const val EXTRA_REQUEST = "request_id"

        /** What the owner typed or ticked and did not send. It stays while the screen is away, for example in the discussion. */
        private val drafts = ConcurrentHashMap<Pair<Long, String>, SheetDraft>()

        fun intent(context: Context, serverId: Long, requestId: String): Intent =
            Intent(context, QuestionActivity::class.java)
                .setAction("app.portalinbox.OPEN_QUESTION:$serverId:$requestId")
                .putExtra(EXTRA_SERVER, serverId)
                .putExtra(EXTRA_REQUEST, requestId)
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

    private var item: Item? = null
    private var sheet: Sheet? = null
    private var draft: SheetDraft? = null
    private var server: Server? = null
    private var shownKey: String? = null
    private var promptSeq = -1L
    private var syncing = false

    private val explanationZone by lazy { ExplanationZone(this, { snackbar(it) }) }

    // views
    private lateinit var content: LinearLayout
    private lateinit var scroller: ScrollView
    private lateinit var hintView: TextView
    private lateinit var linkView: TextView
    private lateinit var declineButton: TextView
    private lateinit var sendButton: TextView
    private lateinit var backButton: TextView
    private lateinit var prompt: BiometricPrompt

    // parts that the refresh updates
    private var statusLine: TextView? = null
    private var notice: NoticeBanner? = null
    private var askCard: View? = null
    private var askTitle: TextView? = null
    private var agentHolder: LinearLayout? = null
    private var agentBoxKey: String? = null
    private val optionViews = ArrayList<OptionView>()
    private val otherViews = ArrayList<OtherView>()
    private val statusViews = HashMap<Int, TextView>()
    private val textFields = HashMap<Int, EditText>()

    private class OptionView(val block: Int, val id: String, val button: CompoundButton)
    private class OtherView(val block: Int, val button: CompoundButton, val field: EditText, val row: View)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        requestId = intent.getStringExtra(EXTRA_REQUEST) ?: ""
        repo = Items.repository(this)
        server = Store.get(this).server(serverId)

        val main = lin()
        main.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }, null))
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

    override fun onResume() {
        super.onResume()
        Registry.addListener(onChange)
        handler.post(tick)
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
        handler.removeCallbacks(tick)
        saveDraft()
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

    private fun saveDraft() {
        val d = draft ?: return
        if (AnswerSheet.empty(sheet ?: return) == d) drafts.remove(serverId to requestId) else drafts[serverId to requestId] = d
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
        declineButton = button(QuestionState.DECLINE_WORD, ButtonKind.SECONDARY) { onDecline() }
        sendButton = button(getString(R.string.question_your_answer), ButtonKind.PRIMARY) { onSend() }
        row.addView(declineButton, lp(0, WRAP, 1f).margins(this, r = 6))
        row.addView(sendButton, lp(0, WRAP, 1.2f).margins(this, l = 6))
        backButton = button(getString(R.string.question_back_to_list), ButtonKind.SECONDARY) { finish() }.apply { visibility = View.GONE }
        row.addView(backButton, lp(0, WRAP, 1f))
        bar.addView(row)
        bar.setBackgroundColor(color(R.color.bg_page))
        return bar
    }

    // ------------------------------------------------------------ refresh

    private fun now(): Long = ServerClock.now(this, serverId)

    private fun agentDirectory() = AgentDirectory(repo.agents(serverId))

    private fun agentBox(i: Item): FromAgentBox.Box = FromAgentBox.of(i, agentDirectory())

    private fun refresh() {
        val current = repo.item(serverId, requestId)
        if (current == null) {
            showGone()
            return
        }
        item = current
        val key = "${current.request.seq}|${current.state}|${current.settled?.id}|${repo.outboxOf(serverId, requestId).count { it.closedAtMs != null }}"
        if (key != shownKey) {
            // A new `request` frame can change the options: the draft of the old ones does not fit.
            val s = AnswerSheet.of(current.request)
            val kept = drafts[serverId to requestId]?.takeIf { d -> s != null && d.blocks.size == s.blocks.size }
            val same = sheet == s
            sheet = s
            draft = if (s == null) null else if (same && draft != null) draft else kept ?: AnswerSheet.empty(s)
            buildContent(current)
            shownKey = key
        }
        refreshDynamic(current)
    }

    private fun showGone() {
        content.removeAllViews()
        content.addView(label(getString(R.string.question_gone), SP_M, color(R.color.text_muted)), lp().margins(this, t = 24))
        hintView.text = ""
        declineButton.visibility = View.GONE
        sendButton.visibility = View.GONE
        backButton.visibility = View.VISIBLE
    }

    private fun screenOf(i: Item): QuestionState.Screen {
        val progress = OutboxAnswer.progress(repo.outboxOf(serverId, requestId))
        return QuestionState.of(
            QuestionState.Input(
                item = i, nowMs = now(), draft = draft, answer = progress, canAuthenticate = LocalAuth.canAuthenticate(this),
                link = ApprovalState.linkOf(Registry.status(serverId).state), portal = server?.nickname ?: "this portal",
                agentLocked = agentBox(i).locked,
                discussed = i.state == ItemState.REPLIED || repo.turns(serverId, requestId).isNotEmpty(),
            ),
        )
    }

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
        val own = QuestionHistory.own(repo.outboxOf(serverId, requestId))
        val now = now()
        statusLine?.text = s.statusLine
        statusLine?.contentDescription = s.statusSpoken

        // The strip at the top: how a closed question ended, an invalid request, the result of an answer.
        val result = (progress as? AnswerProgress.Done)?.let { QuestionHistory.resultLine(it) }
        val closed = s.mode == QuestionState.Mode.CLOSED
        val banner = if (closed) QuestionHistory.banner(i, own, ZoneId.systemDefault(), now) else null
        val tryAgain = (progress as? AnswerProgress.Done)?.let { OutboxAnswer.canTryAgain(it) } == true
        val text = when {
            closed && banner != null && result != null && own == null -> banner.text + "\n" + result
            closed -> banner?.text
            s.notice != null -> s.notice
            tryAgain -> result
            else -> null
        }
        val tone = when {
            s.mode == QuestionState.Mode.INVALID -> ThreadState.Tone.WARNING
            closed -> banner?.tone ?: ThreadState.Tone.MUTED
            else -> ThreadState.Tone.WARNING
        }
        val danger = s.mode == QuestionState.Mode.INVALID
        notice?.show(
            text,
            if (danger) color(R.color.danger) else toneFg(tone),
            if (danger) color(R.color.danger_soft) else toneBg(tone),
            if (tone == ThreadState.Tone.INFO) R.drawable.ic_info else R.drawable.ic_warning,
        )

        askCard?.visibility = if (s.showAsk) View.VISIBLE else View.GONE
        askTitle?.text = s.askWord
        syncAnswers(s)

        // The bottom bar.
        hintView.text = s.bottomHint ?: ""
        hintView.visibility = if (s.bottomHint == null) View.GONE else View.VISIBLE
        linkView.text = s.linkLine ?: ""
        linkView.visibility = if (s.linkLine == null) View.GONE else View.VISIBLE
        linkView.setTextColor(color(if (s.sendLock == QuestionState.Lock.PORTAL_BLOCKED) R.color.danger else R.color.warning))
        declineButton.visibility = if (s.declineShown) View.VISIBLE else View.GONE
        declineButton.alpha = if (s.declineEnabled) 1f else 0.45f
        sendButton.visibility = if (s.sendShown) View.VISIBLE else View.GONE
        backButton.visibility = if (closed) View.VISIBLE else View.GONE
        val on = s.sendEnabled
        sendButton.background = if (on) pressable(color(R.color.primary), dpf(CONTROL_RADIUS))
        else pressable(color(R.color.bg_hover), dpf(CONTROL_RADIUS), color(R.color.hairline))
        sendButton.setTextColor(if (on) color(R.color.on_primary) else color(R.color.text_subtle))
        sendButton.setCompoundDrawablesRelativeWithIntrinsicBounds(
            getDrawable(R.drawable.ic_send)?.mutate()?.apply {
                setTint(if (on) color(R.color.on_primary) else color(R.color.text_subtle))
                setBounds(0, 0, dp(18), dp(18))
            },
            null, null, null,
        )
        sendButton.compoundDrawablePadding = dp(8)
        sendButton.text = sendText(s)
        sendButton.contentDescription = TalkBackLabels.questionSend(s, server?.let { PortalIdentity.of(it).nickname })
    }

    /** "Send answer", and on a second, smaller line "on Production". */
    private fun sendText(s: QuestionState.Screen): CharSequence {
        val second = server?.let { getString(R.string.approval_on_portal, PortalIdentity.of(it).nickname) } ?: return s.sendWord
        return SpannableStringBuilder(s.sendWord).append('\n').append(
            android.text.SpannableString(second).apply { setSpan(RelativeSizeSpan(0.8f), 0, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE) },
        )
    }

    private fun toneFg(t: ThreadState.Tone): Int = when (t) {
        ThreadState.Tone.WARNING -> color(R.color.warning)
        ThreadState.Tone.INFO -> color(R.color.primary)
        ThreadState.Tone.MUTED -> color(R.color.text_muted)
    }

    private fun toneBg(t: ThreadState.Tone): Int = when (t) {
        ThreadState.Tone.WARNING -> color(R.color.warning_soft)
        ThreadState.Tone.INFO -> color(R.color.primary_soft)
        ThreadState.Tone.MUTED -> color(R.color.bg_hover)
    }

    /** Puts the state of the draft on the buttons, the fields and the status of each block. It never rebuilds a view. */
    private fun syncAnswers(s: QuestionState.Screen) {
        val sh = sheet ?: return
        val d = draft ?: return
        syncing = true
        try {
            val editable = s.mode == QuestionState.Mode.OPEN
            for (ov in optionViews) {
                val on = ov.id in d.blocks[ov.block].ticked
                ov.button.isChecked = on
                ov.button.isEnabled = editable
                (ov.button.parent as? View)?.setBackgroundColor(if (on) color(R.color.primary_soft) else 0)
            }
            for (o in otherViews) {
                val bd = d.blocks[o.block]
                o.button.isChecked = bd.otherOn
                o.button.isEnabled = editable
                o.field.isEnabled = editable
                o.row.setBackgroundColor(if (bd.otherOn) color(R.color.primary_soft) else 0)
            }
            for (field in textFields.values) field.isEnabled = editable
            for ((b, view) in statusViews) {
                val st = AnswerSheet.status(sh, d, b)
                view.text = when (st) {
                    BlockStatus.Complete -> getString(R.string.question_answered)
                    BlockStatus.Empty -> getString(R.string.question_not_answered)
                    is BlockStatus.Invalid -> st.reason
                }
                view.setTextColor(
                    when (st) {
                        BlockStatus.Complete -> color(R.color.primary)
                        BlockStatus.Empty -> color(R.color.text_subtle)
                        is BlockStatus.Invalid -> color(R.color.danger)
                    },
                )
            }
        } finally {
            syncing = false
        }
    }

    // ------------------------------------------------------------ content

    private fun buildContent(i: Item) {
        content.removeAllViews()
        statusLine = null; notice = null; askCard = null; agentHolder = null; agentBoxKey = null
        optionViews.clear(); otherViews.clear(); statusViews.clear(); textFields.clear()
        val q = i.request.question
        val s = screenOf(i)

        server?.let { content.addView(PortalBanner.banner(this, it), lp().margins(this, l = -10, r = -10, t = 4)) }
        content.addView(label(ItemTexts.title(i), SP_XL, color(R.color.text_strong), MEDIUM).apply {
            if (Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
            contentDescription = TalkBackLabels.questionHeader(i, now(), agentDirectory().labelOf(i))
        }, lp().margins(this, t = 8, l = 4, r = 4))
        content.addView(
            label(QuestionState.senderLine(i, s.automatic, agentDirectory().labelOf(i)), SP_S, color(R.color.text_muted)),
            lp().margins(this, t = 4, l = 4, r = 4),
        )
        agentHolder = lin()
        content.addView(agentHolder, lp().margins(this, t = 8))
        refreshAgentBox(i)
        val chips = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        val kindChip = if (s.automatic) R.string.chip_decision else R.string.chip_question
        chips.addView(chip(getString(kindChip), color(R.color.primary), color(R.color.primary_soft)))
        statusLine = label(s.statusLine, SP_S, color(R.color.text_muted))
        chips.addView(statusLine, lp(0, WRAP, 1f).margins(this, l = 8))
        content.addView(chips, lp().margins(this, t = 8, l = 4, r = 4))
        ItemTexts.leaseLine(i, ZoneId.systemDefault())?.takeIf { i.isOpen }?.let {
            content.addView(label(it, SP_S, color(R.color.text_subtle)), lp().margins(this, t = 4, l = 4, r = 4))
        }

        notice = NoticeBanner(this)
        content.addView(notice, lp().margins(this, t = 10))

        q?.command?.let { cmd -> content.addView(commandCard(cmd), lp().margins(this, t = 10)) }
        if (q?.explanation != null) {
            (explanationZone.view.parent as? android.view.ViewGroup)?.removeView(explanationZone.view)
            content.addView(explanationZone.view, lp().margins(this, t = 10))
            explanationZone.show(q.explanation, q.explanationFormat)
        }
        s.hint?.let { content.addView(hintCard(it), lp().margins(this, t = 10)) }

        val sh = sheet
        if (!i.isOpen) {
            val own = QuestionHistory.own(repo.outboxOf(serverId, requestId))
            if (own != null && sh != null) {
                val answered = QuestionHistory.answerBlocks(sh, own.frame)
                for (a in answered) content.addView(answeredCard(a, sh.isForm, sh.blocks.size), lp().margins(this, t = 10))
            }
            i.settled?.supersededBy?.takeIf { i.state == ItemState.SUPERSEDED }?.let { newId ->
                val newer = repo.item(serverId, newId)
                if (newer != null && newer.isOpen) {
                    content.addView(textLink(getString(R.string.question_open_new)) { startActivity(ItemScreens.intent(this, newer)) }, lp().margins(this, t = 4))
                }
            }
        } else if (sh != null && s.mode != QuestionState.Mode.INVALID) {
            sh.blocks.forEachIndexed { n, b -> content.addView(blockCard(sh, n, b, s), lp().margins(this, t = 10)) }
        }
        s.closedLine?.let { content.addView(label(it, SP_S, color(R.color.text_muted)), lp().margins(this, t = 10, l = 4, r = 4)) }
        askCard = askCard(s)
        content.addView(askCard, lp().margins(this, t = 10))
        content.addView(View(this), lp(MATCH, dp(16)))
    }

    private fun textLink(value: String, onClick: () -> Unit): TextView =
        label(value, SP_M, color(R.color.primary), MEDIUM).apply {
            minHeight = dp(48)
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(4), 0, dp(12), 0)
            isClickable = true
            isFocusable = true
            background = rowRipple()
            setOnClickListener { onClick() }
        }

    // ------------------------------------------------------------ the command

    /** The exact text of the agent. Copy puts it on the clipboard as it came. Hidden characters show as marks. No read gate. */
    private fun commandCard(command: app.portalinbox.proto.Command): View {
        val language = command.language?.takeIf { it != Language.PLAIN }
        val colored = CodeSpans.build(language?.wire, command.text, NativeRender)
        val marked = VisibleText.render(colored, nonAscii = true)
        val card = lin().apply {
            background = solidCard()
            clipToOutline = true
            outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
        }
        val head = lin(vertical = false, padH = 12, padV = 8).apply { gravity = Gravity.CENTER_VERTICAL }
        val name = command.label?.let { VisibleText.escapeLine(it) }
        head.addView(caption(getString(R.string.question_command) + (name?.let { " ($it)" } ?: "")))
        head.addView(caption(getString(R.string.question_command_from_agent), color(R.color.text_muted)), lp(0, WRAP, 1f).margins(this, l = 8))
        head.addView(copyButton(getString(R.string.approval_copy_description)) {
            copyToClipboard(server?.let { PortalLabels.clipboardLabel(PortalIdentity.of(it)) } ?: "command", command.text)
            snackbar(getString(R.string.approval_copied))
        })
        card.addView(head)
        card.addView(divider())
        val applier = SpanApplier(this, CodePalette.COMMAND)
        val sb = applier.apply(marked.text)
        val text = mono(sb, 13f, color(R.color.text), lines = Int.MAX_VALUE).apply {
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
        val box = GateScrollView(this, dp(9 * 20 + 20)) { _, _, _ -> }
        box.addView(inner, android.view.ViewGroup.LayoutParams(MATCH, WRAP))
        box.contentDescription = getString(R.string.question_command) + ". " + command.text
        card.addView(box, lp())
        card.addView(divider())
        val sqlLike = language == null || language == Language.SQL
        val words = VisibleText.facts(command.text, marked, sqlLike).words().toMutableList()
        if (language != null) words += "colors: ${language.wire} hint, text unchanged"
        val foot = lin(padH = 12, padV = 8)
        foot.addView(mono(words.joinToString("  "), SP_S, color(R.color.text_subtle), lines = 4))
        if (marked.hasMarks) foot.addView(mono("${marked.markCount} marked", SP_S, color(R.color.danger), bold = true))
        card.addView(foot)
        return card
    }

    // ------------------------------------------------------------ the suggestion

    /** The suggestion of the agent or of a system, with its confidence. It never ticks an option: the owner chooses. */
    private fun hintCard(h: QuestionState.HintBox): View {
        val card = lin(padH = 12, padV = 10).apply { background = solidCard() }
        val top = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        top.addView(label(getString(R.string.question_suggestion) + ": " + h.suggested, SP_L, color(R.color.text_strong), MEDIUM), lp(0, WRAP, 1f))
        h.percent?.let { top.addView(label(getString(R.string.question_suggestion_confidence, it), SP_M, color(R.color.text_muted)).apply { fontFeatureSettings = "tnum" }) }
        card.addView(top)
        h.percent?.let {
            card.addView(
                ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
                    max = 100
                    progress = it
                    progressTintList = ColorStateList.valueOf(color(R.color.warning))
                    progressBackgroundTintList = ColorStateList.valueOf(color(R.color.hairline))
                    progressBackgroundTintMode = android.graphics.PorterDuff.Mode.SRC_IN
                },
                lp(MATCH, dp(4)).margins(this, t = 8),
            )
        }
        card.addView(label(h.note, SP_S, color(R.color.text_muted)), lp().margins(this, t = 6))
        card.isFocusable = true
        card.contentDescription = h.spoken
        return card
    }

    // ------------------------------------------------------------ the blocks of the answer

    private fun agentText(text: String): CharSequence = SpanApplier(this).apply(VisibleText.render(text, nonAscii = false).text)

    private fun blockCard(sh: Sheet, n: Int, b: SheetBlock, s: QuestionState.Screen): View {
        val card = lin().apply {
            background = solidCard()
            clipToOutline = true
            outlineProvider = android.view.ViewOutlineProvider.BACKGROUND
        }
        val head = lin(vertical = false, padH = 12, padV = 10).apply { gravity = Gravity.CENTER_VERTICAL }
        if (sh.isForm) {
            head.addView(
                label((n + 1).toString(), SP_M, color(R.color.text_muted), MEDIUM).apply {
                    gravity = Gravity.CENTER
                    background = rounded(color(R.color.field), dpf(14f), color(R.color.hairline), dp(1))
                },
                lp(dp(28), dp(28)).margins(this, r = 8),
            )
            b.header?.let { head.addView(chip(VisibleText.escapeLine(it), color(R.color.primary), color(R.color.primary_soft)), lp(WRAP, WRAP).margins(this, r = 8)) }
        } else {
            head.addView(caption(getString(R.string.question_your_answer)), lp(WRAP, WRAP).margins(this, r = 8))
        }
        head.addView(label(AnswerSheet.chooseLine(b), SP_S, color(R.color.text_muted)), lp(0, WRAP, 1f))
        val status = label("", SP_S, color(R.color.text_subtle), MEDIUM).apply { gravity = Gravity.END }
        statusViews[n] = status
        if (sh.isForm) head.addView(status, lp(WRAP, WRAP).margins(this, l = 8))
        head.isFocusable = true
        head.contentDescription = TalkBackLabels.block(n + 1, sh.blocks.size, b.header, AnswerSheet.chooseLine(b), false)
        card.addView(head)
        b.text?.let { card.addView(label(agentText(it), SP_L, color(R.color.text_strong)).apply { setPadding(dp(12), 0, dp(12), dp(10)) }) }
        card.addView(divider())

        if (b.kind == BlockKind.TEXT) {
            card.addView(textBlock(n, b), lp().margins(this, t = 10, l = 10, r = 10, b = 10))
            return card
        }
        for ((k, o) in b.options.withIndex()) {
            if (k > 0) card.addView(divider(insetStart = 56))
            card.addView(optionRow(n, b, o))
        }
        if (s.showOther && b.otherAllowed) {
            card.addView(divider())
            card.addView(otherRow(n, b, sh.isForm))
        }
        return card
    }

    private fun newButton(b: SheetBlock): CompoundButton =
        (if (b.kind == BlockKind.CHECKBOX) CheckBox(this) else RadioButton(this)).apply {
            textSize = SP_L
            setTextColor(color(R.color.text))
            buttonTintList = ColorStateList.valueOf(color(R.color.primary))
            minHeight = dp(56)
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(8), dp(8), dp(12), dp(8))
        }

    private fun optionText(o: InputOption): CharSequence {
        val sb = SpannableStringBuilder(agentText(o.label))
        o.description?.let {
            val start = sb.length + 1
            sb.append('\n').append(agentText(it))
            sb.setSpan(RelativeSizeSpan(0.82f), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            sb.setSpan(ForegroundColorSpan(color(R.color.text_muted)), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        }
        return sb
    }

    private fun optionRow(n: Int, b: SheetBlock, o: InputOption): View {
        val button = newButton(b).apply {
            text = optionText(o)
            setOnClickListener { onTap(n, o.id) }
        }
        optionViews += OptionView(n, o.id, button)
        return lin().apply { addView(button, lp(MATCH, WRAP)) }
    }

    /** The row "Other answer": a field, the warning against a secret. A key that is closed has no such row ([QuestionState]). */
    private fun otherRow(n: Int, b: SheetBlock, isForm: Boolean): View {
        val note = QuestionState.otherNote(b.kind, isForm)
        val wrap = lin()
        val button = newButton(b).apply {
            val sb = SpannableStringBuilder(QuestionState.OTHER_WORD)
            if (b.kind == BlockKind.CHECKBOX || isForm) sb.append("  ").append(getString(R.string.question_exclusive).uppercase())
            val start = sb.length + 1
            sb.append('\n').append(note)
            sb.setSpan(RelativeSizeSpan(0.82f), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            sb.setSpan(ForegroundColorSpan(color(R.color.text_muted)), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            text = sb
            contentDescription = TalkBackLabels.otherRow(note)
            setOnClickListener { onTapOther(n) }
        }
        wrap.addView(button, lp(MATCH, WRAP))
        val field = textField(QuestionState.OTHER_WORD).apply {
            setText(draft?.blocks?.get(n)?.otherText ?: "")
            hint = getString(R.string.question_other_hint)
            addTextChangedListener(watcher { text -> onTypeOther(n, text) })
        }
        wrap.addView(field, lp().margins(this, l = 52, r = 12, b = 4))
        wrap.addView(label(QuestionState.SECRET_WARNING, SP_S, color(R.color.warning)), lp().margins(this, l = 52, r = 12, b = 12))
        otherViews += OtherView(n, button, field, wrap)
        return wrap
    }

    private fun textBlock(n: Int, b: SheetBlock): View {
        val wrap = lin()
        val field = textField(b.text ?: getString(R.string.question_type_your_answer)).apply {
            setText(draft?.blocks?.get(n)?.text ?: "")
            hint = b.placeholder?.let { VisibleText.escapeLine(it) } ?: getString(R.string.question_type_your_answer)
            addTextChangedListener(watcher { text -> onTypeText(n, text) })
        }
        textFields[n] = field
        wrap.addView(field, lp())
        wrap.addView(label(QuestionState.SECRET_WARNING, SP_S, color(R.color.warning)), lp().margins(this, t = 6))
        return wrap
    }

    private fun textField(description: String): EditText = EditText(this).apply {
        textSize = SP_L
        minHeight = dp(48)
        minLines = 2
        gravity = Gravity.TOP
        inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
        // The field stops at the limit of the server: nothing is cut afterwards.
        filters = arrayOf(InputFilter.LengthFilter(AnswerSheet.MAX_TEXT_CHARS))
        setTextColor(color(R.color.text))
        setHintTextColor(color(R.color.text_subtle))
        background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
        setPadding(dp(12), dp(10), dp(12), dp(10))
        contentDescription = TalkBackLabels.questionField(description)
    }

    private fun watcher(onText: (String) -> Unit) = object : TextWatcher {
        override fun afterTextChanged(s: android.text.Editable?) {
            if (!syncing) onText(s?.toString() ?: "")
        }

        override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
        override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
    }

    // ------------------------------------------------------------ the answered blocks (a closed question)

    private fun answeredCard(a: QuestionHistory.BlockAnswer, isForm: Boolean, total: Int): View {
        val card = lin(padH = 12, padV = 10).apply { background = solidCard() }
        val head = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        if (isForm) {
            head.addView(
                label((a.index + 1).toString(), SP_M, color(R.color.text_muted), MEDIUM).apply {
                    gravity = Gravity.CENTER
                    background = rounded(color(R.color.field), dpf(14f), color(R.color.hairline), dp(1))
                },
                lp(dp(28), dp(28)).margins(this, r = 8),
            )
            a.header?.let { head.addView(chip(VisibleText.escapeLine(it), color(R.color.primary), color(R.color.primary_soft))) }
        } else {
            head.addView(caption(getString(R.string.question_your_answer)))
        }
        card.addView(head)
        a.question?.let { card.addView(label(agentText(it), SP_L, color(R.color.text_strong)), lp().margins(this, t = 8)) }
        if (a.free != null) {
            card.addView(
                label(agentText(a.free), SP_L, color(R.color.text)).apply {
                    background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
                    setPadding(dp(12), dp(10), dp(12), dp(10))
                },
                lp().margins(this, t = 8),
            )
            if (a.kind != BlockKind.TEXT) card.addView(label(getString(R.string.question_your_own_answer), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4))
        } else {
            for (o in a.chosen) {
                val row = lin(vertical = false).apply { gravity = Gravity.TOP }
                row.addView(icon(R.drawable.ic_check, color(R.color.primary), 18), lp(dp(18), dp(18)).margins(this, t = 2, r = 10))
                row.addView(label(optionText(o), SP_L, color(R.color.text)), lp(0, WRAP, 1f))
                card.addView(row, lp().margins(this, t = 8))
            }
            if (a.notChosen.isNotEmpty()) {
                card.addView(
                    label(getString(R.string.question_not_chosen, a.notChosen.joinToString(", ") { VisibleText.escapeLine(it.label) }), SP_S, color(R.color.text_muted)),
                    lp().margins(this, t = 8),
                )
            }
        }
        card.isFocusable = true
        return card
    }

    // ------------------------------------------------------------ the row "Ask the agent"

    private fun askCard(s: QuestionState.Screen): View {
        val card = lin().apply {
            background = solidCard()
            visibility = if (s.showAsk) View.VISIBLE else View.GONE
        }
        val row = lin(vertical = false, padH = 12, padV = 10).apply {
            minimumHeight = dp(56)
            isClickable = true
            isFocusable = true
            background = rowRipple()
            contentDescription = QuestionState.ASK_WORD + ". " + QuestionState.ASK_NOTE
            setOnClickListener { startActivity(ThreadActivity.intent(this@QuestionActivity, serverId, requestId)) }
        }
        row.addView(icon(R.drawable.ic_help, color(R.color.text_muted), 22), lp(dp(22), dp(22)).margins(this, r = 14))
        val texts = lin()
        askTitle = label(s.askWord, SP_L, color(R.color.text_strong))
        texts.addView(askTitle)
        texts.addView(label(QuestionState.ASK_NOTE, SP_S, color(R.color.text_muted)), lp().margins(this, t = 2))
        row.addView(texts, lp(0, WRAP, 1f))
        card.addView(row)
        return card
    }

    // ------------------------------------------------------------ what the owner does

    private fun change(next: SheetDraft?) {
        next ?: return
        draft = next
        saveDraft()
        item?.let { refreshDynamic(it) }
    }

    private fun onTap(block: Int, optionId: String) {
        val sh = sheet ?: return
        val d = draft ?: return
        if (AnswerSheet.atMax(sh, d, block, optionId)) snackbar(getString(R.string.question_max_reached))
        change(AnswerSheet.tap(sh, d, block, optionId))
    }

    private fun onTapOther(block: Int) {
        val sh = sheet ?: return
        val d = draft ?: return
        change(AnswerSheet.tapOther(sh, d, block))
    }

    private fun onTypeOther(block: Int, text: String) {
        val sh = sheet ?: return
        val d = draft ?: return
        change(AnswerSheet.typeOther(sh, d, block, text))
    }

    private fun onTypeText(block: Int, text: String) {
        val sh = sheet ?: return
        val d = draft ?: return
        change(AnswerSheet.typeText(sh, d, block, text))
    }

    private fun onDecline() {
        val i = item ?: return
        if (!screenOf(i).declineEnabled) return
        afterSubmit(i, Items.engine(this).decline(serverId, requestId))
    }

    private fun onSend() {
        val i = item ?: return
        val s = screenOf(i)
        if (!s.sendEnabled) {
            s.bottomHint?.let { snackbar(it) }
            return
        }
        if (!LocalAuth.canAuthenticate(this)) {
            snackbar(getString(R.string.question_no_lock))
            refresh()
            return
        }
        // The owner confirms for this question: the version of the request that is on the screen now.
        promptSeq = i.request.seq
        val info = BiometricPrompt.PromptInfo.Builder()
            .setTitle(BiometricPolicy.answerTitle(server?.nickname ?: "this portal"))
            .setSubtitle(BiometricPolicy.answerSubtitle(i.request.question?.title ?: ""))
            .setAllowedAuthenticators(LocalAuth.authenticators())
            .build()
        prompt.authenticate(info)
    }

    /** The local gate said yes. The answer goes out only if the question is still the one that the owner answered. */
    private fun onAuthenticated() {
        val i = repo.item(serverId, requestId)
        val d = draft
        if (i == null || !i.isOpen || i.request.seq != promptSeq || d == null) {
            snackbar(getString(R.string.question_changed))
            refresh()
            return
        }
        afterSubmit(i, Items.engine(this).answerQuestion(serverId, requestId, d))
    }

    private fun afterSubmit(i: Item, r: Submit) {
        OutboxAnswer.submitLine(r)?.let { snackbar(it) }
        if (r is Submit.Queued) {
            Registry.connections[serverId]?.flushOutbox()
            ItemAlerter.withdrawNow(this, i)
            drafts.remove(serverId to requestId)
        }
        Registry.changed()
        refresh()
    }
}
