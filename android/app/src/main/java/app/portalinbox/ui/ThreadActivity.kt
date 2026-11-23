package app.portalinbox.ui

import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.InputFilter
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import app.portalinbox.R
import app.portalinbox.alert.ItemAlerter
import app.portalinbox.core.NativeRender
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Item
import app.portalinbox.data.ItemRepository
import app.portalinbox.data.ReplyOutbox
import app.portalinbox.data.Submit
import app.portalinbox.proto.Command
import app.portalinbox.proto.ExplanationFormat
import app.portalinbox.proto.Language
import app.portalinbox.proto.RequestKind
import app.portalinbox.render.CodeSpans
import app.portalinbox.render.MarkdownSpans
import app.portalinbox.render.MdBlock
import app.portalinbox.service.Items
import app.portalinbox.service.Registry
import app.portalinbox.service.ServerClock
import java.time.ZoneId
import java.util.concurrent.ConcurrentHashMap

/**
 * The discussion with an agent. It only draws. The decisions are in [ThreadState] (the thread, the field,
 * the end states), [ReplyOutbox] (the message of the owner) and [TalkBackLabels] (the words of a screen reader).
 *
 * The turns of the agent are texts of a third party: a dashed edge, the label "Not checked", Markdown with the colors of the
 * muted palette, a Copy button on each block of code, and the marks for hidden characters. The messages of the owner are on
 * the right. The field below is blocked while the agent prepares its reply, and when the item is closed.
 */
class ThreadActivity : BaseActivity() {
    companion object {
        private const val EXTRA_SERVER = "server_id"
        private const val EXTRA_REQUEST = "request_id"

        /** The text of a field that the owner did not send, kept while the screen is away (not across a restart of the app). */
        private val drafts = ConcurrentHashMap<Pair<Long, String>, String>()

        fun intent(context: Context, serverId: Long, requestId: String): Intent =
            Intent(context, ThreadActivity::class.java)
                .setAction("app.portalinbox.OPEN_THREAD:$serverId:$requestId")
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

    private lateinit var scroller: ScrollView
    private lateinit var content: LinearLayout
    private lateinit var codeLabel: TextView
    private lateinit var hintView: TextView
    private lateinit var field: EditText
    private lateinit var sendButton: View
    private var statusView: TextView? = null
    private var lastScreen: ThreadState.Screen? = null
    private var firstBuild = true

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        requestId = intent.getStringExtra(EXTRA_REQUEST) ?: ""
        repo = Items.repository(this)

        val main = lin()
        codeLabel = mono("", SP_S, color(R.color.text_muted), bold = true)
        main.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }, codeLabel))
        content = lin(padH = 10, padV = 4)
        scroller = scroll(content)
        main.addView(scroller, lp(MATCH, 0, 1f))
        main.addView(bottomBar())
        setScreen(main)
        field.setText(savedInstanceState?.getString("draft") ?: drafts[serverId to requestId] ?: "")
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString("draft", field.text.toString())
    }

    override fun onResume() {
        super.onResume()
        OpenScreens.enterThread(serverId, requestId)
        ItemAlerter.withdrawReply(this, serverId, requestId)
        Registry.addListener(onChange)
        handler.post(tick)
    }

    override fun onPause() {
        super.onPause()
        OpenScreens.leaveThread(serverId, requestId)
        Registry.removeListener(onChange)
        handler.removeCallbacks(tick)
        val text = field.text.toString()
        if (text.isEmpty()) drafts.remove(serverId to requestId) else drafts[serverId to requestId] = text
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

    // ------------------------------------------------------------ bottom bar: the hint and the field

    private fun bottomBar(): View {
        val bar = lin(padH = 10, padV = 6)
        bar.addView(divider(), lp(MATCH, 1).margins(this, b = 4))
        hintView = label("", SP_S, color(R.color.text_muted)).apply { gravity = Gravity.CENTER }
        bar.addView(hintView, lp().margins(this, t = 2, b = 6))
        val row = lin(vertical = false).apply { gravity = Gravity.BOTTOM }
        field = EditText(this).apply {
            textSize = SP_L
            minHeight = dp(48)
            maxLines = 5
            gravity = Gravity.CENTER_VERTICAL
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            // The field stops at the limit: nothing is cut afterwards.
            filters = arrayOf(InputFilter.LengthFilter(ReplyOutbox.MAX_CHARS))
            setTextColor(color(R.color.text))
            setHintTextColor(color(R.color.text_subtle))
            background = rounded(color(R.color.field), dpf(CONTROL_RADIUS), color(R.color.glass_stroke), dp(1))
            setPadding(dp(14), dp(10), dp(14), dp(10))
            addTextChangedListener(object : android.text.TextWatcher {
                override fun afterTextChanged(s: android.text.Editable?) {
                    lastScreen?.let { updateComposer(it.composer) }
                }

                override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
                override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
            })
        }
        row.addView(field, lp(0, WRAP, 1f))
        sendButton = android.widget.FrameLayout(this).apply {
            isClickable = true
            isFocusable = true
            minimumWidth = dp(48)
            minimumHeight = dp(48)
            setOnClickListener { onSend() }
            addView(
                icon(R.drawable.ic_send, color(R.color.on_primary), 22),
                android.widget.FrameLayout.LayoutParams(dp(22), dp(22), Gravity.CENTER),
            )
        }
        row.addView(sendButton, lp(dp(52), dp(52)).margins(this, l = 8))
        bar.addView(row)
        bar.setBackgroundColor(color(R.color.bg_page))
        return bar
    }

    private fun updateComposer(c: ThreadState.Composer) {
        field.isEnabled = c.enabled
        field.alpha = if (c.enabled) 1f else 0.55f
        field.hint = c.placeholder
        field.contentDescription = TalkBackLabels.composer(c)
        val hasText = field.text.isNotBlank()
        val on = c.enabled && hasText
        sendButton.background = if (on) pressable(color(R.color.primary), dpf(CONTROL_RADIUS))
        else pressable(color(R.color.bg_hover), dpf(CONTROL_RADIUS), color(R.color.hairline))
        (sendButton as android.widget.FrameLayout).getChildAt(0).let {
            (it as android.widget.ImageView).imageTintList = android.content.res.ColorStateList.valueOf(
                if (on) color(R.color.on_primary) else color(R.color.text_subtle),
            )
        }
        sendButton.contentDescription = TalkBackLabels.sendButton(c, hasText)
        hintView.text = c.hint ?: ""
        hintView.visibility = if (c.hint == null) View.GONE else View.VISIBLE
    }

    // ------------------------------------------------------------ the owner sends

    private fun onSend() {
        val c = lastScreen?.composer ?: return
        if (!c.enabled) {
            c.hint?.let { snackbar(it) }
            return
        }
        when (val d = ReplyOutbox.check(field.text.toString())) {
            is ReplyOutbox.Draft.Refused -> snackbar(d.reason)
            is ReplyOutbox.Draft.Ok -> {
                val r = Items.engine(this).reply(serverId, requestId, d.text)
                ReplyOutbox.submitLine(r)?.let { snackbar(it) }
                if (r is Submit.Queued) {
                    field.setText("")
                    drafts.remove(serverId to requestId)
                    Registry.connections[serverId]?.flushOutbox()
                    stickToBottom = true
                }
                Registry.changed()
                refresh()
            }
        }
    }

    // ------------------------------------------------------------ refresh

    private fun now(): Long = ServerClock.now(this, serverId)

    private var stickToBottom = false

    private fun atBottom(): Boolean {
        val child = scroller.getChildAt(0) ?: return true
        return scroller.scrollY + scroller.height >= child.height - dp(24)
    }

    private fun refresh() {
        val item = repo.item(serverId, requestId)
        if (item == null) {
            showGone()
            return
        }
        val offset = now() - System.currentTimeMillis()
        val screen = ThreadState.of(
            ThreadState.Input(
                item, repo.turns(serverId, requestId), repo.outboxOf(serverId, requestId), now(), offset, ZoneId.systemDefault(),
                agentName = AgentDirectory(repo.agents(serverId)).labelOf(item),
            ),
        )
        codeLabel.text = item.request.approval?.code ?: ""
        val previous = lastScreen
        // The status line changes with the clock: it does not rebuild the thread.
        if (previous == null || previous.copy(statusLine = "", statusSpoken = "") != screen.copy(statusLine = "", statusSpoken = "")) {
            val follow = firstBuild || stickToBottom || atBottom()
            buildContent(item, screen)
            if (follow) scroller.post { scroller.fullScroll(View.FOCUS_DOWN) }
            firstBuild = false
            stickToBottom = false
        }
        statusView?.text = screen.statusLine
        lastScreen = screen
        updateComposer(screen.composer)
    }

    private fun showGone() {
        content.removeAllViews()
        content.addView(label(getString(R.string.thread_gone), SP_M, color(R.color.text_muted)), lp().margins(this, t = 24))
        field.isEnabled = false
        hintView.visibility = View.GONE
        sendButton.visibility = View.GONE
    }

    // ------------------------------------------------------------ content

    private fun buildContent(item: Item, s: ThreadState.Screen) {
        content.removeAllViews()
        statusView = null
        content.addView(label(s.title, SP_XL, color(R.color.text_strong), MEDIUM).apply {
            if (Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
            contentDescription = TalkBackLabels.threadHeader(s)
        }, lp().margins(this, t = 8, l = 4, r = 4))
        content.addView(label(s.senderLine, SP_S, color(R.color.text_muted)), lp().margins(this, t = 4, l = 4, r = 4))
        val chips = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        s.chip?.let { chips.addView(chip(it.word, toneFg(it.tone), toneBg(it.tone))) }
        if (s.statusLine.isNotEmpty()) {
            val status = label(s.statusLine, SP_S, color(R.color.text_muted))
            statusView = status
            chips.addView(status, lp(0, WRAP, 1f).margins(this, l = if (s.chip == null) 0 else 8))
        }
        if (chips.childCount > 0) content.addView(chips, lp().margins(this, t = 6, l = 4, r = 4))

        s.banner?.let { b ->
            val banner = NoticeBanner(this)
            banner.show(
                b.text, toneFg(b.tone), toneBg(b.tone),
                if (b.tone == ThreadState.Tone.INFO) R.drawable.ic_info else R.drawable.ic_warning,
            )
            content.addView(banner, lp().margins(this, t = 10))
        }
        if (item.kind == RequestKind.APPROVAL) content.addView(openRequestLink(item), lp().margins(this, t = 4))
        s.replacedBy?.let { newId ->
            val newer = repo.item(serverId, newId)
            if (newer != null && newer.isOpen && newer.kind == RequestKind.APPROVAL) {
                content.addView(textLink(getString(R.string.thread_open_new)) {
                    startActivity(ApprovalActivity.intent(this, serverId, newId))
                }, lp().margins(this, t = 4))
            }
        }

        s.opening?.let { content.addView(openingBubble(s, it, item.isOpen && !ItemTexts.isAutomatic(item)), lp().margins(this, t = 14)) }
        for (e in s.entries) content.addView(entryView(e, s.agentLabel), lp().margins(this, t = 14))
        if (s.writing) content.addView(writingRow(s.agentLabel), lp().margins(this, t = 14, b = 8, l = 4))
        content.addView(View(this), lp(MATCH, dp(12)))
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

    /** The link to the request screen of an approval. The screen is already below this one when the owner came from it. */
    private fun openRequestLink(item: Item): View = textLink(getString(R.string.thread_open_request)) {
        startActivity(
            ApprovalActivity.intent(this, serverId, requestId)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        )
    }.apply {
        contentDescription = getString(R.string.thread_open_request) + " " + (item.request.approval?.code ?: "")
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

    // ------------------------------------------------------------ the turns

    private fun whoRow(name: String, whenText: String, agent: Boolean, markdown: Boolean, gravityEnd: Boolean): LinearLayout =
        lin(vertical = false, padH = 4).apply {
            gravity = (if (gravityEnd) Gravity.END else Gravity.START) or Gravity.CENTER_VERTICAL
            addView(label(name, SP_M, color(R.color.text_strong), MEDIUM))
            addView(label(whenText, SP_S, color(R.color.text_subtle)), lp(WRAP, WRAP).margins(this@ThreadActivity, l = 8))
            if (agent) {
                addView(caption(getString(R.string.thread_not_checked), color(R.color.warning)), lp(WRAP, WRAP).margins(this@ThreadActivity, l = 8))
                if (markdown) addView(caption(getString(R.string.thread_markdown), color(R.color.text_muted)), lp(WRAP, WRAP).margins(this@ThreadActivity, l = 8))
            }
        }

    private fun entryView(e: ThreadState.Entry, agent: String): View = when (e) {
        is ThreadState.Entry.Agent -> agentTurn(e, agent)
        is ThreadState.Entry.User -> userTurn(e)
    }

    private fun agentTurn(e: ThreadState.Entry.Agent, agent: String): View {
        val box = lin()
        box.addView(whoRow(agent, e.whenText, agent = true, markdown = e.format == ExplanationFormat.MARKDOWN, gravityEnd = false))
        box.addView(agentBubble(e.explanation, e.format, e.command), lp().margins(this, t = 4))
        box.isFocusable = true
        box.contentDescription = TalkBackLabels.turn(e, agent, listOfNotNull(e.explanation, e.command?.text).joinToString(". "))
        return box
    }

    /** The bubble of the text of a third party: a dashed edge, the muted palette, a Copy button on each block of code. */
    private fun agentBubble(explanation: String?, format: ExplanationFormat?, command: Command?): View {
        val bubble = lin(padH = 12, padV = 10).apply {
            background = dashedBox(color(R.color.card), color(R.color.card_stroke), dpf(CARD_RADIUS))
        }
        val applier = SpanApplier(this, CodePalette.AGENT)
        if (explanation != null) {
            if (format == ExplanationFormat.MARKDOWN) {
                val doc = MarkdownSpans.build(explanation, NativeRender, NativeRender)
                for (block in doc.blocks) bubble.addView(markdownBlock(block, applier), lp().margins(this, b = 4))
                if (doc.fallback) {
                    bubble.addView(label("Shown as plain text: it is too long or too deep.", SP_S, color(R.color.text_muted)), lp().margins(this, b = 4))
                }
            } else {
                bubble.addView(plainText(explanation, applier), lp().margins(this, b = 4))
            }
        }
        if (command != null) bubble.addView(commandBlock(command), lp().margins(this, t = if (explanation != null) 6 else 0, b = 2))
        return bubble
    }

    private fun plainText(text: String, applier: SpanApplier): TextView =
        label(applier.apply(VisibleText.render(text, nonAscii = false).text), SP_L, color(R.color.text)).apply {
            setTextIsSelectable(false)
            isLongClickable = false
            setLineSpacing(dpf(2f), 1f)
        }

    private fun markdownBlock(block: MdBlock, applier: SpanApplier): View = when (block) {
        is MdBlock.Flow -> label(applier.apply(VisibleText.render(block.text, nonAscii = false).text), SP_L, color(R.color.text)).apply {
            setTextIsSelectable(false)
            isLongClickable = false
            movementMethod = null
            setLineSpacing(dpf(2f), 1f)
        }
        is MdBlock.Code -> agentCodeBlock(
            block.lang, VisibleText.render(block.text, nonAscii = false).text,
            { text ->
                copyToClipboard("code", text)
                snackbar(getString(R.string.thread_code_copied))
            },
            block.text.text,
        )
    }

    /** The command of a turn of the agent: a block of code of the agent (no line numbers, muted colors, Copy). */
    private fun commandBlock(command: Command): View {
        val language = command.language?.takeIf { it != Language.PLAIN }?.wire
        val colored = CodeSpans.build(language, command.text, NativeRender)
        val shown = VisibleText.render(colored, nonAscii = false).text
        return agentCodeBlock(command.label ?: language ?: "plain", shown, { text ->
            copyToClipboard("command", text)
            snackbar(getString(R.string.thread_code_copied))
        }, command.text)
    }

    private fun userTurn(e: ThreadState.Entry.User): View {
        val box = lin().apply { gravity = Gravity.END }
        box.addView(whoRow(getString(R.string.thread_you), e.whenText, agent = false, markdown = false, gravityEnd = true))
        val widest = (resources.displayMetrics.widthPixels * 0.82f).toInt()
        val text = label(
            SpanApplier(this).apply(VisibleText.render(e.text, nonAscii = false).text), SP_L, color(R.color.text_strong),
        ).apply {
            setTextIsSelectable(false)
            setLineSpacing(dpf(2f), 1f)
            maxWidth = widest
            setPadding(dp(14), dp(10), dp(14), dp(10))
            val failed = e.delivery == ThreadState.Delivery.NOT_SENT
            background = rounded(
                if (failed) color(R.color.danger_soft) else color(R.color.primary_soft), dpf(CONTROL_RADIUS + 4),
                (if (failed) color(R.color.danger) else color(R.color.primary)) and 0x66FFFFFF, dp(1),
            )
            alpha = if (e.delivery == ThreadState.Delivery.QUEUED || e.delivery == ThreadState.Delivery.SENT) 0.8f else 1f
        }
        box.addView(text, lp(WRAP, WRAP).margins(this, t = 4).apply { gravity = Gravity.END })
        e.line?.let {
            val failed = e.delivery == ThreadState.Delivery.NOT_SENT
            box.addView(
                label(it, SP_S, if (failed) color(R.color.danger) else color(R.color.text_muted)).apply { gravity = Gravity.END },
                lp(WRAP, WRAP).margins(this, t = 4, l = 40).apply { gravity = Gravity.END },
            )
        }
        box.isFocusable = true
        box.contentDescription = TalkBackLabels.turn(e, "", e.text)
        return box
    }

    private fun writingRow(agent: String): View = lin(vertical = false).apply {
        gravity = Gravity.CENTER_VERTICAL
        contentDescription = TalkBackLabels.writing(agent)
        isFocusable = true
        addView(label(agent, SP_M, color(R.color.text_strong), MEDIUM))
        addView(label(getString(R.string.thread_writing), SP_S, color(R.color.text_muted)), lp(WRAP, WRAP).margins(this@ThreadActivity, l = 8))
        addView(label("•••", SP_S, color(R.color.text_subtle)), lp(WRAP, WRAP).margins(this@ThreadActivity, l = 8))
    }

    /** The first bubble of a question: its text and its choices. The choices do not answer here: a link opens the screen of the question. */
    private fun openingBubble(s: ThreadState.Screen, o: ThreadState.Opening, open: Boolean): View {
        val box = lin()
        box.addView(whoRow(s.agentLabel, o.whenText, agent = true, markdown = o.format == ExplanationFormat.MARKDOWN, gravityEnd = false))
        val bubble = agentBubble(o.explanation, o.format, o.command) as LinearLayout
        bubble.addView(label(o.title, SP_L, color(R.color.text_strong), MEDIUM), 0, lp().margins(this, b = 6))
        if (o.options.isNotEmpty()) {
            bubble.addView(
                label(getString(R.string.thread_choices, o.options.joinToString(" · ")), SP_S, color(R.color.text_muted)),
                lp().margins(this, t = 6),
            )
            if (open) {
                bubble.addView(
                    textLink(getString(R.string.question_answer_to_agent)) {
                        startActivity(
                            QuestionActivity.intent(this, serverId, requestId)
                                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
                        )
                    },
                    lp().margins(this, t = 2),
                )
            }
        }
        box.addView(bubble, lp().margins(this, t = 4))
        box.isFocusable = true
        box.contentDescription = "${s.agentLabel}, ${o.whenText}, not checked. ${o.title}"
        return box
    }
}
