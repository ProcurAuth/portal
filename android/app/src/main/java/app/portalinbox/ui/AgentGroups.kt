package app.portalinbox.ui

import app.portalinbox.data.Agent
import app.portalinbox.data.AgentDirectory
import app.portalinbox.data.AgentKey
import app.portalinbox.data.Item
import app.portalinbox.proto.AgentKind
import app.portalinbox.proto.AgentState
import app.portalinbox.proto.Observed

/**
 * The section "To answer", grouped by agent (lot G4). Pure Kotlin: the list gives the open items that [app.portalinbox.portal.ItemSort]
 * sorted (the oldest first, across portals), the directory of the agents, and the folded groups. The result is what the screen draws.
 *
 * Rules:
 * - A group is one agent of one portal. Two portals never share a group, even when their agent ids are equal.
 * - The agent of an item is the agent of `from.agent_id`, after the merges. The name comes from the table of the agents, so a rename
 *   shows on old items at once. The frame of the item gives the name only when the phone does not hold the agent.
 * - The order of the groups: the new agents first, then the groups by their oldest open item, the bucket last. Inside a group the
 *   items keep the order that they came in: the oldest first.
 * - The phone counts the open items itself. The `open` of the frame is a snapshot and is not used.
 * - Folding is local: it changes the rows that show, never the count.
 */
object AgentGroups {
    data class Group(
        /** [AgentKey.of] of the portal and the agent after the merges. */
        val key: String,
        val serverId: Long,
        val agentId: String,
        /** The name of the agent: text of the portal. */
        val name: String,
        /** The name of the key: text of the portal. */
        val keyName: String,
        val kind: AgentKind,
        val state: AgentState,
        /** The owner gave a name. A new agent with no name shows "not named". */
        val named: Boolean,
        /** The time of the last call on the clock of the portal, or null when the phone does not hold the agent. */
        val lastCallMs: Long?,
        val observed: Observed?,
        /** The agent as the table holds it, or null when its frame did not arrive yet. */
        val agent: Agent?,
        val items: List<Item>,
        val folded: Boolean,
        /** The line under the rows of a new agent that the server locks, or null. */
        val lockLine: String?,
    ) {
        val count: Int get() = items.size
        val isNew: Boolean get() = state == AgentState.NEW && kind == AgentKind.AGENT
        val isBucket: Boolean get() = kind == AgentKind.BUCKET

        /** The rows that show: none when the group is folded. */
        val shown: List<Item> get() = if (folded) emptyList() else items
    }

    /**
     * Makes the groups. [items] are open items, the oldest first. Items that are not open are ignored. [collapse] gives the folded
     * groups. [directory] gives the agents.
     */
    fun build(items: List<Item>, directory: AgentDirectory, collapse: AgentCollapse): List<Group> {
        val order = LinkedHashMap<String, MutableList<Item>>()
        val ids = HashMap<String, Pair<Long, String>>()
        for (item in items) {
            if (!item.isOpen) continue
            val resolved = directory.agentOf(item)
            val key = AgentKey.of(item.serverId, resolved.agentId)
            order.getOrPut(key) { ArrayList() }.add(item)
            ids[key] = item.serverId to resolved.agentId
        }
        val groups = order.entries.mapIndexed { index, (key, rows) ->
            val (serverId, agentId) = ids.getValue(key)
            val agent = directory.get(serverId, agentId)
            val from = rows.first().request.from
            val kind = agent?.kind ?: from.agentKind
            val state = agent?.state ?: from.agentState
            val group = Group(
                key = key, serverId = serverId, agentId = agentId,
                name = VisibleText.escapeLine(agent?.label ?: from.label),
                keyName = VisibleText.escapeLine(agent?.keyName ?: from.keyName),
                kind = kind, state = state, named = agent?.named ?: false,
                lastCallMs = agent?.lastSeenMs, observed = if (kind == AgentKind.BUCKET) null else agent?.observed, agent = agent,
                items = rows, folded = collapse.isFolded(key),
                lockLine = if (agent?.locksAccept == true) AgentTexts.LOCK_LINE else null,
            )
            index to group
        }
        // New agents first, the bucket last, the rest by the oldest open item. `index` is the place of the oldest item.
        return groups.sortedWith(
            compareBy<Pair<Int, Group>>({ if (it.second.isNew) 0 else if (it.second.isBucket) 2 else 1 }, { it.first }),
        ).map { it.second }
    }

    /** The keys of the groups, for [AgentCollapse.retain]. */
    fun keys(groups: List<Group>): List<String> = groups.map { it.key }

    // ------------------------------------------------------------ the header

    /** "ops-key · last call 1 min ago", "ops-key · agent not known", "ops-key · new, not named". [nowMs] is the clock of the portal. */
    fun keyLine(g: Group, nowMs: Long): String = when {
        g.isBucket -> "${g.keyName} · agent not known"
        g.isNew -> g.keyName + " · " + if (g.named) "new" else "new, not named"
        g.lastCallMs != null -> "${g.keyName} · last call ${Ages.ago(g.lastCallMs, nowMs)}"
        else -> g.keyName
    }

    /** What the hint line under the header of a group says: its label, and the parts. Null when the group has no hint. */
    data class Hint(val label: String, val parts: List<String>, val untrusted: Boolean)

    /** The hint line. A bucket gets "NOT KNOWN". An agent gets "OBSERVED" with the parts that the client sent: they are not verified. */
    fun hint(g: Group): Hint? = when {
        g.isBucket -> Hint("NOT KNOWN", listOf(AgentTexts.BUCKET_HINT), false)
        else -> AgentTexts.observedParts(g.observed).takeIf { it.isNotEmpty() }?.let { Hint("OBSERVED", it, true) }
    }

    /** The label of a header for a screen reader. */
    fun spoken(g: Group, nowMs: Long): String {
        val state = AgentTexts.stateSpoken(g.state, g.kind)
        val last = g.lastCallMs?.let { ", last call " + Ages.spoken(it, nowMs) } ?: ""
        val count = if (g.count == 1) "1 request" else "${g.count} requests"
        val fold = if (g.folded) "folded" else "open"
        return "Agent ${g.name}, key ${g.keyName}, $state$last, $count, $fold. Double tap to ${if (g.folded) "open" else "fold"}."
    }
}
