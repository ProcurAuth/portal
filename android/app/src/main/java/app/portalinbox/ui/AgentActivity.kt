package app.portalinbox.ui

import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.View
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import app.portalinbox.R
import app.portalinbox.data.Agent
import app.portalinbox.data.AgentActions
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Store
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.proto.AgentOp
import app.portalinbox.proto.AgentState
import app.portalinbox.service.AgentNotices
import app.portalinbox.service.Items
import app.portalinbox.service.Registry
import app.portalinbox.service.ServerClock
import java.time.ZoneId

/**
 * The screen of one agent, and the list of the agents of one portal (lot G4). It only draws: the facts are in [AgentScreenModel], the
 * actions in [AgentActions]. The name, the key and the state are text of the portal. The box "Observed, not verified" is text of a client.
 * An action goes out as an `agent_do` frame: no biometric check, because an action on an agent never runs a command. The result of the
 * portal comes back as a line in a snackbar.
 */
class AgentActivity : BaseActivity() {
    companion object {
        private const val EXTRA_SERVER = "server_id"
        private const val EXTRA_AGENT = "agent_id"

        /** The screen of one agent. Only the app opens it: the activity is not exported. */
        fun intent(context: Context, serverId: Long, agentId: String): Intent =
            Intent(context, AgentActivity::class.java)
                .setAction("app.portalinbox.OPEN_AGENT:$serverId:$agentId")
                .putExtra(EXTRA_SERVER, serverId)
                .putExtra(EXTRA_AGENT, agentId)

        /** The list of the agents of one portal, where an owner can find a blocked agent to unblock it. */
        fun list(context: Context, serverId: Long): Intent =
            Intent(context, AgentActivity::class.java)
                .setAction("app.portalinbox.OPEN_AGENTS:$serverId")
                .putExtra(EXTRA_SERVER, serverId)
    }

    private var serverId = -1L
    private var agentId: String? = null
    private lateinit var bar: FrameLayout
    private lateinit var body: LinearLayout
    private val handler = Handler(Looper.getMainLooper())
    private val onChange: () -> Unit = {
        render()
        showAgentNotices()
    }
    private val tick = object : Runnable {
        override fun run() {
            render()
            handler.postDelayed(this, 30_000)
        }
    }

    // The text that the owner typed: a redraw never loses it.
    private var renameDraft: String? = null
    private var allowDraft = ""
    private var typing = false
    private var redrawAfterTyping = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        agentId = intent.getStringExtra(EXTRA_AGENT)
        renameDraft = savedInstanceState?.getString("rename")
        allowDraft = savedInstanceState?.getString("allow") ?: ""
        if (Store.get(this).server(serverId) == null) return finish()
        val root = lin()
        bar = FrameLayout(this)
        root.addView(bar, lp())
        body = lin(padH = 10, padV = 4)
        root.addView(scroll(body), lp(MATCH, 0, 1f))
        setScreen(root)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        renameDraft?.let { outState.putString("rename", it) }
        outState.putString("allow", allowDraft)
    }

    override fun onResume() {
        super.onResume()
        Registry.addListener(onChange)
        render()
        showAgentNotices()
        handler.post(tick)
    }

    override fun onPause() {
        super.onPause()
        Registry.removeListener(onChange)
        handler.removeCallbacks(tick)
    }

    private fun showAgentNotices() {
        while (true) {
            val line = AgentNotices.take() ?: return
            snackbar(line)
        }
    }

    private fun render() {
        if (typing) {
            if (currentFocus is EditText) {
                redrawAfterTyping = true
                return
            }
            typing = false
        }
        val store = Store.get(this)
        val server = store.server(serverId) ?: return finish()
        val repo = Items.repository(this)
        val directory = AgentDirectory(repo.agents(serverId))
        bar.removeAllViews()
        bar.addView(topBar(R.drawable.ic_arrow_left, getString(R.string.nav_back), { finish() }, null))
        body.removeAllViews()
        // The portal comes first, as on every screen of an item: the nickname is the identity.
        body.addView(portalChip(PortalIdentity.of(server)), lp(WRAP, WRAP).margins(this, t = 4, l = 4))
        val id = agentId
        if (id == null) {
            title = getString(R.string.agents_title)
            renderList(directory)
            return
        }
        val agent = directory.get(serverId, id)
        if (agent == null) {
            body.addView(label(getString(R.string.agent_not_found), SP_M, color(R.color.text_muted)), lp().margins(this, t = 24))
            return
        }
        title = getString(R.string.agent_screen_title)
        renderAgent(agent, directory)
    }

    // ------------------------------------------------------------ the list

    private fun renderList(directory: AgentDirectory) {
        body.addView(label(getString(R.string.agents_title), SP_XL, color(R.color.text_strong), MEDIUM).apply {
            if (android.os.Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
        }, lp().margins(this, t = 8, l = 4, r = 4))
        body.addView(label(getString(R.string.agents_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 4, l = 4, r = 4, b = 8))
        val agents = directory.all.sortedWith(compareBy({ rank(it) }, { it.label.lowercase() }, { it.agentId }))
        if (agents.isEmpty()) {
            body.addView(label(getString(R.string.agents_empty), SP_M, color(R.color.text_muted)), lp().margins(this, t = 16, l = 4))
            return
        }
        val panel = panel()
        agents.forEachIndexed { i, a ->
            if (i > 0) panel.addView(divider())
            panel.addView(
                row(VisibleText.escapeLine(a.label), VisibleText.escapeLine(a.keyName), agentStateChip(a.state, a.kind)) {
                    startActivity(intent(this, serverId, a.agentId))
                }.apply {
                    minimumHeight = dp(52)
                    contentDescription = VisibleText.escapeLine(a.label) + ", key " + VisibleText.escapeLine(a.keyName) + ", " + AgentTexts.stateSpoken(a.state, a.kind)
                },
            )
        }
        body.addView(panel)
    }

    /** New agents first, then the live ones, the blocked ones, the merged ones. */
    private fun rank(a: Agent): Int = when {
        a.isNew -> 0
        a.state == AgentState.KNOWN -> if (a.isBucket) 2 else 1
        a.state == AgentState.BLOCKED -> 3
        else -> 4
    }

    // ------------------------------------------------------------ one agent

    private fun renderAgent(agent: Agent, directory: AgentDirectory) {
        val repo = Items.repository(this)
        val now = ServerClock.now(this, serverId)
        val m = AgentScreenModel.of(agent, directory, repo.items(serverId), now, ZoneId.systemDefault(), AgentCommands.pending(this, serverId, agent.agentId))

        body.addView(label(m.name, SP_XL, color(R.color.text_strong), MEDIUM).apply {
            if (android.os.Build.VERSION.SDK_INT >= 28) isAccessibilityHeading = true
        }, lp().margins(this, t = 8, l = 4, r = 4))
        body.addView(label(m.keyLine, SP_S, color(R.color.text_muted)), lp().margins(this, t = 4, l = 4, r = 4))
        val chips = lin(vertical = false).apply { gravity = Gravity.CENTER_VERTICAL }
        chips.addView(agentStateChip(agent.state, agent.kind))
        chips.addView(label(m.factsLine, SP_S, color(R.color.text_muted)), lp(0, WRAP, 1f).margins(this, l = 8))
        body.addView(chips, lp().margins(this, t = 8, l = 4, r = 4))
        m.mergedNote?.let { body.addView(label(it, SP_S, color(R.color.text_muted)), lp().margins(this, t = 8, l = 4, r = 4)) }
        if (m.isBucket) body.addView(label(getString(R.string.agent_bucket_help), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8, l = 4, r = 4))
        if (m.pending.isNotEmpty()) {
            body.addView(label(getString(R.string.agent_sending), SP_S, color(R.color.warning), MEDIUM), lp().margins(this, t = 8, l = 4, r = 4))
        }

        if (!m.isBucket) {
            val warnings = if (agent.isNew) AgentTexts.warnings(agent, directory) else emptyList()
            body.addView(observedBox(m.observed, warnings, m.noProof), lp().margins(this, t = 12))
        }
        m.lockLine?.let { body.addView(agentLockLine(it), lp().margins(this, t = 8)) }

        if (AgentOp.ALLOW in m.actions) renderAllow(agent, m.allowAsLabel)
        renderOpen(m)
        if (AgentOp.RENAME in m.actions) renderRename(agent)
        if (AgentOp.MERGE in m.actions || (!m.isBucket && agent.isLive && AgentOp.MERGE in m.pending)) renderMerge(m)
        if (AgentOp.BLOCK in m.actions) renderBlock(agent)
        if (AgentOp.UNBLOCK in m.actions) renderUnblock(agent)
        body.addView(View(this), lp(MATCH, dp(24)))
    }

    private fun section(title: String): LinearLayout {
        body.addView(sectionTitle(title))
        return panel(padH = 14, padV = 12).also { body.addView(it) }
    }

    private fun onTyping(has: Boolean) {
        typing = has
        if (!has && redrawAfterTyping) {
            redrawAfterTyping = false
            handler.post { render() }
        }
    }

    private fun renderAllow(agent: Agent, allowAsLabel: String) {
        val p = section(getString(R.string.agent_allow))
        p.addView(nameField(getString(R.string.agent_name_hint), allowDraft) { allowDraft = it }.apply {
            setOnFocusChangeListener { _, has -> onTyping(has) }
        })
        p.addView(button(getString(R.string.agent_name_and_allow), ButtonKind.PRIMARY) {
            when (val c = AgentActions.checkName(allowDraft)) {
                is AgentActions.NameCheck.Ok -> send(agent, AgentOp.ALLOW, name = c.name)
                else -> snackbar(AgentActions.nameLine(c) ?: "")
            }
        }, lp().margins(this, t = 10))
        p.addView(button(allowAsLabel, ButtonKind.SECONDARY) { send(agent, AgentOp.ALLOW) }, lp().margins(this, t = 8))
    }

    private fun renderOpen(m: AgentScreenModel.Model) {
        body.addView(sectionTitle(getString(R.string.agent_open_requests, m.open.size)))
        val p = panel()
        if (m.open.isEmpty()) {
            p.addView(label(getString(R.string.agent_no_open_requests), SP_M, color(R.color.text_muted)).apply { setPadding(dp(14), dp(12), dp(14), dp(12)) })
        }
        val directory = AgentDirectory(Items.repository(this).agents(serverId))
        m.open.forEachIndexed { i, item ->
            if (i > 0) p.addView(divider())
            val at = ServerClock.now(this, item.serverId)
            val r = ItemListOrder.row(item, at, directory.labelOf(item), grouped = true)
            val chipText = getString(if (r.chip == ItemListOrder.Chip.APPROVAL) R.string.chip_approval else R.string.chip_question)
            p.addView(
                row(r.title, r.subtitle.ifEmpty { r.detail }, chip(chipText, color(R.color.primary), color(R.color.primary_soft))) {
                    if (r.openable) startActivity(ApprovalActivity.intent(this, item.serverId, item.requestId))
                }.apply { minimumHeight = dp(52) },
            )
        }
        body.addView(p)
    }

    private fun renderRename(agent: Agent) {
        val p = section(getString(R.string.agent_rename_title))
        val draft = renameDraft ?: if (agent.named) agent.label else ""
        p.addView(nameField(getString(R.string.agent_name_hint), draft) { renameDraft = it }.apply {
            setOnFocusChangeListener { _, has -> onTyping(has) }
        })
        p.addView(button(getString(R.string.agent_rename_save), ButtonKind.SECONDARY) {
            when (val c = AgentActions.checkName(renameDraft ?: draft)) {
                is AgentActions.NameCheck.Ok -> send(agent, AgentOp.RENAME, name = c.name)
                else -> snackbar(AgentActions.nameLine(c) ?: "")
            }
        }, lp().margins(this, t = 10))
    }

    private fun renderMerge(m: AgentScreenModel.Model) {
        val p = section(getString(R.string.agent_merge_title))
        p.addView(label(getString(R.string.agent_merge_help), SP_S, color(R.color.text_muted)))
        if (m.mergeTargets.isEmpty()) {
            p.addView(label(getString(R.string.agent_merge_none), SP_S, color(R.color.text_muted)), lp().margins(this, t = 8))
            return
        }
        p.addView(button(getString(R.string.agent_merge_button), ButtonKind.SECONDARY) {
            val similar = m.agent.similar?.agentId
            val choices = m.mergeTargets.map { NewAgentCard.MergeChoice(it.agentId, VisibleText.escapeLine(it.label), it.agentId == similar) }
                .sortedBy { !it.looksLike }
            agentMergeSheet(choices) { into -> send(m.agent, AgentOp.MERGE, into = into) }
        }, lp().margins(this, t = 10))
    }

    private fun renderBlock(agent: Agent) {
        val p = section(getString(R.string.agent_block_title))
        p.addView(label(getString(R.string.agent_block_help), SP_S, color(R.color.text_muted)))
        p.addView(button(getString(R.string.agent_block), ButtonKind.DANGER) { agentBlockSheet { send(agent, AgentOp.BLOCK) } }, lp().margins(this, t = 10))
    }

    private fun renderUnblock(agent: Agent) {
        val p = section(getString(R.string.agent_unblock))
        p.addView(label(getString(R.string.agent_unblock_help), SP_S, color(R.color.text_muted)))
        p.addView(button(getString(R.string.agent_unblock), ButtonKind.SECONDARY) { send(agent, AgentOp.UNBLOCK) }, lp().margins(this, t = 10))
    }

    private fun send(agent: Agent, op: AgentOp, name: String? = null, into: String? = null) {
        val refusal = AgentCommands.run(this, agent.serverId, agent.agentId, op, name, into)
        if (refusal != null) {
            snackbar(refusal)
            return
        }
        when (op) {
            AgentOp.ALLOW -> allowDraft = ""
            AgentOp.RENAME -> renameDraft = null
            else -> {}
        }
        render()
    }
}
