package app.portalinbox.ui

import app.portalinbox.data.Agent
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Item
import app.portalinbox.proto.AgentKind
import app.portalinbox.proto.AgentState

/**
 * The box "From agent" under the title of the approval screen (lot G4). Pure Kotlin.
 *
 * The name, the state and the key are text of the portal. The line "Not verified: Says it is ..." is text that the agent sent. It
 * goes through [UntrustedText] on the screen, like the explanation. A request that says another name than the agent said before
 * is shown too: a change of what the agent says is a sign to read.
 */
object FromAgentBox {
    data class Box(
        val name: String,
        val stateWord: String,
        val stateSpoken: String,
        val keyName: String,
        val isNew: Boolean,
        val isBucket: Boolean,
        /** What the agent says that it is: the table first (the last value that the portal saw), else the text of the request. Untrusted. */
        val says: String?,
        /** The text of this request when it differs from [says]. Untrusted. */
        val saysHere: String?,
        /** The warnings of a new agent (same text as a name). Portal text only. */
        val warnings: List<String>,
        /** Approve is locked: the agent is new and the server locks the new agents. */
        val locked: Boolean,
        /** The agent of the item, after the merges, or null when the phone does not hold it. */
        val agent: Agent?,
        val agentId: String,
    ) {
        /** True when the screen offers the link that opens the agent screen: the phone holds the agent. */
        val canOpenAgent: Boolean get() = agent != null
    }

    /**
     * The box for [item]. The state comes from the table: the `from.agent_state` of an old frame is only the fallback, so an agent that the
     * owner allowed after the request arrived does not stay "new".
     */
    fun of(item: Item, directory: AgentDirectory): Box {
        val r = directory.agentOf(item)
        val agent = r.agent
        val from = item.request.from
        val kind = agent?.kind ?: from.agentKind
        val state = agent?.state ?: from.agentState
        val says = agent?.observed?.says ?: item.request.agentLabel
        val here = item.request.agentLabel
        return Box(
            name = VisibleText.escapeLine(agent?.label ?: from.label),
            stateWord = AgentTexts.stateWord(state, kind), stateSpoken = AgentTexts.stateSpoken(state, kind),
            keyName = VisibleText.escapeLine(agent?.keyName ?: from.keyName),
            isNew = state == AgentState.NEW && kind == AgentKind.AGENT, isBucket = kind == AgentKind.BUCKET,
            says = if (kind == AgentKind.BUCKET) null else says,
            saysHere = if (kind != AgentKind.BUCKET && here != null && here != agent?.observed?.says && agent?.observed?.says != null) here else null,
            warnings = if (agent != null && agent.isNew) AgentTexts.warnings(agent, directory) else emptyList(),
            locked = agent?.locksAccept == true,
            agent = agent, agentId = r.agentId,
        )
    }

    /** The words of the line before the text: "Not verified: Says it is". The text itself is marked by the screen. */
    const val SAYS_PREFIX = "Says it is"
    const val NOT_VERIFIED = "Not verified"
}
