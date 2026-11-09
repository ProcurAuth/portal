package app.portalinbox.ui

import app.portalinbox.data.Agent
import app.portalinbox.data.AgentActions
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Item
import app.portalinbox.proto.AgentKind
import app.portalinbox.proto.AgentOp
import app.portalinbox.proto.AgentState
import app.portalinbox.proto.ItemState
import java.time.ZoneId

/**
 * The agent screen (lot G4): what a tap on a group header shows. Pure Kotlin. The name, the key and the state are text of the
 * portal. The box of hints is text of a client, marked "Not verified". The actions are the actions that the portal accepts for the
 * state of the agent: see [AgentActions.available].
 */
object AgentScreenModel {
    data class Model(
        val serverId: Long,
        val agentId: String,
        val name: String,
        /** "Agent of key ops-key". */
        val keyLine: String,
        val stateWord: String,
        val stateSpoken: String,
        /** "Last call 1 min ago · 2 open · 14 answered". */
        val factsLine: String,
        val observed: List<AgentTexts.ObservedRow>,
        val noProof: String,
        /** The agents that this one can merge into (same key and portal). */
        val mergeTargets: List<Agent>,
        val actions: Set<AgentOp>,
        val pending: Set<AgentOp>,
        /** The open items of the agent, the oldest first. */
        val open: List<Item>,
        val isBucket: Boolean,
        val lockLine: String?,
        /** "This agent was merged into Billing bot." for a merged agent: the screen then shows the target. */
        val mergedNote: String?,
        /** The agent that the screen is about, after the merges: the agent that is asked to act. */
        val agent: Agent,
    )

    /**
     * The model for [agent]. [items] are the items of the portal (open and final): the phone counts them. [nowMs] is the clock of
     * the portal. [pending] are the actions that wait in the outbox.
     */
    fun of(agent: Agent, directory: AgentDirectory, items: Collection<Item>, nowMs: Long, zone: ZoneId, pending: Set<AgentOp>): Model {
        val mine = items.filter { it.serverId == agent.serverId && directory.resolve(it.serverId, it.request.from.agentId).agentId == agent.agentId }
        val open = mine.filter { it.isOpen }.sortedWith(compareBy({ it.request.createdAtMs }, { it.requestId }))
        val answered = mine.count { it.state == ItemState.ANSWERED }
        val targets = directory.mergeTargets(agent)
        val facts = buildList {
            if (!agent.isBucket) add("Last call " + Ages.ago(agent.lastSeenMs, nowMs))
            add("${open.size} open")
            add("$answered answered")
        }.joinToString(" · ")
        val merged = agent.mergedInto?.let { into ->
            directory.get(agent.serverId, into)?.let { "This agent was merged into ${VisibleText.escapeLine(it.label)}." } ?: "This agent was merged into another agent."
        }
        return Model(
            serverId = agent.serverId, agentId = agent.agentId, name = VisibleText.escapeLine(agent.label),
            keyLine = if (agent.kind == AgentKind.BUCKET) "Items of key ${VisibleText.escapeLine(agent.keyName)} that no agent claims"
            else "Agent of key ${VisibleText.escapeLine(agent.keyName)}",
            stateWord = AgentTexts.stateWord(agent.state, agent.kind), stateSpoken = AgentTexts.stateSpoken(agent.state, agent.kind),
            factsLine = facts,
            observed = if (agent.isBucket) emptyList() else AgentTexts.observedRows(agent.observed, detail = true),
            noProof = AgentTexts.NO_PROOF, mergeTargets = targets, actions = AgentActions.available(agent, targets) - pending,
            pending = pending, open = open, isBucket = agent.isBucket,
            lockLine = if (agent.locksAccept) AgentTexts.LOCK_LINE else null,
            mergedNote = merged.takeIf { agent.state == AgentState.MERGED }, agent = agent,
        )
    }
}
