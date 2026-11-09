package app.portalinbox.data

import app.portalinbox.proto.AgentFrame
import app.portalinbox.proto.AgentKind
import app.portalinbox.proto.AgentRef
import app.portalinbox.proto.AgentState
import app.portalinbox.proto.MAX_SYNC_AGENTS
import app.portalinbox.proto.Observed
import app.portalinbox.proto.Similar

/**
 * One agent as the phone holds it: the last `agent` frame with the highest `rev`, for one portal. The key of the table is
 * [serverId] and [agentId]: two portals can hold agents with the same id, and they stay apart.
 *
 * [open] and [lastSeenMs] are the values of the frame when the portal sent it. They are snapshots: the phone counts the open
 * items itself ([AgentDirectory.openCount]). [label] and [keyName] are text of the portal. [observed] is text of a client:
 * it is not verified, and a screen shows it only in a box marked "Not verified".
 */
data class Agent(
    val serverId: Long,
    val agentId: String,
    val rev: Long,
    val keyName: String,
    val kind: AgentKind,
    val state: AgentState,
    val label: String,
    val named: Boolean,
    val mergedInto: String?,
    val createdAtMs: Long,
    val lastSeenMs: Long,
    val open: Long,
    val lockAccept: Boolean,
    val observed: Observed?,
    val similar: Similar?,
    /** The phone time when the frame arrived. */
    val receivedAtMs: Long,
    /** True when the phone already counted this agent in the notification. It never turns false again. */
    val notified: Boolean,
) {
    val isBucket: Boolean get() = kind == AgentKind.BUCKET
    val isNew: Boolean get() = state == AgentState.NEW

    /** The server refuses `accept` for the items of this agent: the owner must allow the agent first. */
    val locksAccept: Boolean get() = state == AgentState.NEW && lockAccept

    /** True for an agent that the owner can still allow, rename, block or merge. */
    val isLive: Boolean get() = state == AgentState.NEW || state == AgentState.KNOWN

    /** The frame as the portal sent it. The delivery id is [deliveryId]. */
    fun toFrame(deliveryId: String): AgentFrame = AgentFrame(
        deliveryId, agentId, rev, keyName, kind, state, label, named, mergedInto, createdAtMs, lastSeenMs, open, lockAccept,
        observed, similar,
    )

    companion object {
        /** The delivery id of a frame that the phone rebuilds from its table: the delivery is over, the id means nothing. */
        const val NO_DELIVERY = "00000000000000000000000000000000"

        fun of(serverId: Long, f: AgentFrame, receivedAtMs: Long, notified: Boolean = false) = Agent(
            serverId, f.agentId, f.rev, f.keyName, f.kind, f.state, f.label, f.named, f.mergedInto, f.createdAtMs, f.lastSeenMs,
            f.open, f.lockAccept, f.observed, f.similar, receivedAtMs, notified,
        )
    }
}

/** The key of an agent in a local table or a set: the portal and the agent. The portal id is part of it (several portals). */
object AgentKey {
    fun of(serverId: Long, agentId: String): String = "$serverId:$agentId"
}

/**
 * The agents of the phone, for the screens that need to look one up. Pure Kotlin. Build it once for a screen refresh from the
 * rows of the table. A lookup never reads the network and never writes.
 */
class AgentDirectory(agents: Collection<Agent>) {
    private val byKey: Map<String, Agent> = agents.associateBy { AgentKey.of(it.serverId, it.agentId) }

    val all: Collection<Agent> get() = byKey.values

    fun get(serverId: Long, agentId: String): Agent? = byKey[AgentKey.of(serverId, agentId)]

    /**
     * The agent that [agentId] stands for now. It follows `merged_into` from agent to agent, until it finds an agent that is
     * not merged. [Resolved.agent] is null when the phone does not hold that agent (its frame did not arrive yet): the id is
     * still the id of the group. A loop in the chain, or a chain longer than [MAX_HOPS], stops at the agent where it was
     * found: the phone never loops.
     */
    fun resolve(serverId: Long, agentId: String): Resolved {
        var id = agentId
        val seen = HashSet<String>()
        repeat(MAX_HOPS) {
            val a = get(serverId, id) ?: return Resolved(id, null)
            val next = a.mergedInto
            if (a.state != AgentState.MERGED || next == null || !seen.add(id)) return Resolved(id, a)
            id = next
        }
        return Resolved(id, get(serverId, id))
    }

    data class Resolved(val agentId: String, val agent: Agent?)

    companion object {
        /** The longest chain of merges that the phone follows. The portal keeps the chain flat, so one hop is the normal case. */
        const val MAX_HOPS = 16
    }

    /** The agent of an item, after the merges. */
    fun agentOf(item: Item): Resolved = resolve(item.serverId, item.request.from.agentId)

    /**
     * The name of the agent of an item. The table gives it, so a rename shows on the old items at once. `from.label` of the
     * frame is only the fallback, for an agent that the phone does not hold yet.
     */
    fun labelOf(item: Item): String = agentOf(item).agent?.label ?: item.request.from.label

    /** The agents of the same key on the same portal that [of] can merge into: live, not a bucket, not [of] itself. The known agents come first. */
    fun mergeTargets(of: Agent): List<Agent> = byKey.values
        .filter { it.serverId == of.serverId && it.keyName == of.keyName && it.agentId != of.agentId && !it.isBucket && it.isLive }
        .sortedWith(compareBy<Agent>({ it.state != AgentState.KNOWN }, { it.label.lowercase() }, { it.agentId }))

    /** The number of open items that belong to [agent] now, after the merges. The phone counts them itself. */
    fun openCount(agent: Agent, items: Collection<Item>): Int =
        items.count { it.isOpen && it.serverId == agent.serverId && resolve(it.serverId, it.request.from.agentId).agentId == agent.agentId }

    /** The agents that wait for a first decision of the owner: state `new`, never a bucket. */
    fun newAgents(): List<Agent> = byKey.values.filter { it.isNew && !it.isBucket }
        .sortedWith(compareBy({ it.createdAtMs }, { it.serverId }, { it.agentId }))
}

/** The `agents` list of the `sync` frame. */
object AgentSync {
    /**
     * The agents that the phone holds, with their `rev`. At most [MAX_SYNC_AGENTS]. When more are held, the agents that can
     * still ask (new, known) go first, the most recently seen first. A blocked or merged agent goes last. Null for none: the
     * member `agents` is then absent.
     */
    fun refs(held: Collection<Agent>): List<AgentRef>? {
        if (held.isEmpty()) return null
        return held.sortedWith(compareBy<Agent>({ !it.isLive }, { -it.lastSeenMs }, { it.agentId }))
            .take(MAX_SYNC_AGENTS).map { AgentRef(it.agentId, it.rev) }
    }
}
