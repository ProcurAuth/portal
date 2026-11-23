package app.portalinbox.ui

import app.portalinbox.data.Agent
import app.portalinbox.data.AgentActions
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.Item
import app.portalinbox.proto.AgentOp
import java.time.Instant
import java.time.ZoneId

/**
 * The card "New agent" above "To answer". Pure Kotlin. One card for each agent in state `new` that the phone holds. It
 * names the facts, shows the hints in a box that says "Not verified", and gives the choices: name and allow, allow with the
 * name of the portal, block, merge.
 *
 * Nothing here comes from the text of an agent except the values of [Model.observed], which the screen marks with [UntrustedText].
 * The lock line is in the model only when the server locks the new agents (`lock_accept` of the frame): by default it does not.
 */
object NewAgentCard {
    /** An agent of the same key that the new agent can merge into. [looksLike]: the portal named it as `similar`, or its name equals the text of the new agent. */
    data class MergeChoice(val agentId: String, val label: String, val looksLike: Boolean)

    data class Model(
        val serverId: Long,
        val agentId: String,
        val keyName: String,
        /** "A new agent is using the key ops-key". */
        val title: String,
        /** "First call 14:19 · 1 request waits". */
        val subtitle: String,
        /** The rows of the box "Observed, not verified". The values are text of a client. */
        val observed: List<AgentTexts.ObservedRow>,
        /** The warnings, then the closing sentence. Empty when nothing looks wrong. */
        val warnings: List<String>,
        /** The line that says that anyone with the key can send these texts. It is always there: the card has hints. */
        val noProof: String,
        /** The name that the portal gave: "ops-key #4". Portal text. */
        val portalLabel: String,
        val choices: List<MergeChoice>,
        /** The line about the lock, or null when the server does not lock the new agents. */
        val lockLine: String?,
        /** What waits in the outbox for this agent: the buttons for these actions show "Sending". */
        val pending: Set<AgentOp>,
    ) {
        val canMerge: Boolean get() = choices.isNotEmpty()

        /** The label of the button that allows with the name of the portal: Allow as "ops-key #4". */
        val allowAsLabel: String get() = "Allow as \"$portalLabel\""

        /** The button to merge: it names the first choice that looks like the new agent. */
        val mergeLabel: String get() = choices.firstOrNull { it.looksLike }?.let { "It is ${it.label}: merge" } ?: "Merge into an agent of this key"
    }

    /**
     * The cards, in the order of the first call. [items] are the open items of the portals that show. [pending] are the actions that
     * wait in the outbox, by portal and agent id. A bucket and an agent that is not `new` have no card.
     */
    fun build(
        directory: AgentDirectory, items: Collection<Item>, nowMs: (Long) -> Long, zone: ZoneId,
        pending: (Long, String) -> Set<AgentOp> = { _, _ -> emptySet() },
        shows: (Long) -> Boolean = { true },
    ): List<Model> = directory.newAgents().filter { shows(it.serverId) }.map { of(it, directory, items, nowMs(it.serverId), zone, pending(it.serverId, it.agentId)) }

    fun of(agent: Agent, directory: AgentDirectory, items: Collection<Item>, nowMs: Long, zone: ZoneId, pending: Set<AgentOp>): Model {
        val waits = directory.openCount(agent, items)
        val first = firstCall(agent.createdAtMs, nowMs, zone)
        val waiting = when (waits) {
            0 -> "no request waits"
            1 -> "1 request waits"
            else -> "$waits requests wait"
        }
        val warnings = AgentTexts.warnings(agent, directory)
        val matched = AgentTexts.nameMatches(agent.observed?.says, agent, directory).map { it.agent.agentId }.toSet() + setOfNotNull(agent.similar?.agentId)
        val choices = directory.mergeTargets(agent).filter { it.state == app.portalinbox.proto.AgentState.KNOWN || it.agentId in matched }
            .map { MergeChoice(it.agentId, VisibleText.escapeLine(it.label), it.agentId in matched) }
            .sortedWith(compareBy({ !it.looksLike }, { it.label.lowercase() }, { it.agentId }))
        return Model(
            serverId = agent.serverId, agentId = agent.agentId, keyName = VisibleText.escapeLine(agent.keyName),
            title = "A new agent is using the key " + VisibleText.escapeLine(agent.keyName),
            subtitle = "First call $first · $waiting",
            observed = AgentTexts.observedRows(agent.observed),
            warnings = warnings, noProof = AgentTexts.NO_PROOF,
            portalLabel = VisibleText.escapeLine(agent.label), choices = choices,
            lockLine = if (agent.locksAccept) AgentTexts.LOCK_LINE else null,
            pending = pending,
        )
    }

    /** "today 14:19", or "11-04 14:19" for another day. [nowMs] is the clock of the portal. */
    fun firstCall(createdAtMs: Long, nowMs: Long, zone: ZoneId): String {
        val stamp = Ages.stamp(createdAtMs, nowMs, zone)
        val same = Instant.ofEpochMilli(createdAtMs).atZone(zone).toLocalDate() == Instant.ofEpochMilli(nowMs).atZone(zone).toLocalDate()
        return if (same) "today $stamp" else stamp
    }

    /** The result of the form of the card: the name that goes with "Name and allow", or the reason that it does not go. */
    fun nameAndAllow(raw: String): AgentActions.NameCheck = AgentActions.checkName(raw)
}
