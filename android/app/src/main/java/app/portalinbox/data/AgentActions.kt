package app.portalinbox.data

import app.portalinbox.proto.AgentDoFrame
import app.portalinbox.proto.AgentOp
import app.portalinbox.proto.AgentResultFrame
import app.portalinbox.proto.AgentResultStatus
import app.portalinbox.proto.AgentState
import app.portalinbox.proto.FrameException
import app.portalinbox.proto.MAX_AGENT_NAME_CHARS

/**
 * What the owner can do with an agent, and what the phone says about it. Pure Kotlin: no Context, no clock.
 *
 * The rules follow the engine of the portal (`agent_do`). The portal decides: the phone only hides the action that the portal
 * would refuse. The device asks for no biometric check here: an action on an agent never runs a command. Only an approval needs it.
 */
object AgentActions {
    /** The actions that make sense for [agent] now. A bucket has none. */
    fun available(agent: Agent, mergeTargets: List<Agent>): Set<AgentOp> {
        if (agent.isBucket) return emptySet()
        return buildSet {
            when (agent.state) {
                AgentState.NEW -> { add(AgentOp.ALLOW); add(AgentOp.RENAME); add(AgentOp.BLOCK) }
                AgentState.KNOWN -> { add(AgentOp.RENAME); add(AgentOp.BLOCK) }
                AgentState.BLOCKED -> { add(AgentOp.UNBLOCK); add(AgentOp.RENAME) }
                AgentState.MERGED -> {}
            }
            if (agent.isLive && mergeTargets.isNotEmpty()) add(AgentOp.MERGE)
        }
    }

    /** The result of a check of a name that the owner typed. */
    sealed interface NameCheck {
        /** The name to send. The edges are trimmed: the owner never means a space there. */
        data class Ok(val name: String) : NameCheck

        data object Empty : NameCheck
        data object TooLong : NameCheck
        data object BadCharacter : NameCheck
    }

    /**
     * Checks a name before it goes out: 1 to [MAX_AGENT_NAME_CHARS] characters, no control, no format character, no line
     * separator. The portal checks again, and also checks that the name is not taken. The phone does not clean the inside of
     * the name: a bad character gives [NameCheck.BadCharacter].
     */
    fun checkName(raw: String): NameCheck {
        val text = raw.trim()
        if (text.isEmpty()) return NameCheck.Empty
        if (text.codePointCount(0, text.length) > MAX_AGENT_NAME_CHARS) return NameCheck.TooLong
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            val type = Character.getType(cp)
            val bad = type == Character.CONTROL.toInt() || type == Character.FORMAT.toInt() ||
                type == Character.LINE_SEPARATOR.toInt() || type == Character.PARAGRAPH_SEPARATOR.toInt() ||
                type == Character.SURROGATE.toInt() || type == Character.UNASSIGNED.toInt()
            if (bad) return NameCheck.BadCharacter
            i += Character.charCount(cp)
        }
        return NameCheck.Ok(text)
    }

    /** The words for a refused name, or null for a good one. */
    fun nameLine(c: NameCheck): String? = when (c) {
        is NameCheck.Ok -> null
        NameCheck.Empty -> "Write a name first."
        NameCheck.TooLong -> "Use $MAX_AGENT_NAME_CHARS characters at most."
        NameCheck.BadCharacter -> "The name has a character that is not allowed."
    }

    /** What an `agent_do` needs: the frame to queue, or the reason that the phone does not make it. */
    sealed interface Plan {
        data class Send(val frame: AgentDoFrame) : Plan
        data class Refused(val reason: Refusal) : Plan
    }

    /**
     * Plans one action. [agent] is the agent that the phone holds. [target] is the agent of a merge. [msgId] is the new
     * `client_msg_id`. The frame is checked by its own rules before it is returned.
     */
    fun plan(agent: Agent?, op: AgentOp, name: String?, target: Agent?, mergeTargets: List<Agent>, msgId: String): Plan {
        if (agent == null) return Plan.Refused(Refusal.NOT_FOUND)
        if (op == AgentOp.MERGE) {
            if (!agent.isLive || agent.isBucket) return Plan.Refused(Refusal.BAD_STATE)
        } else if (op !in available(agent, mergeTargets)) {
            return Plan.Refused(Refusal.BAD_STATE)
        }
        val frame = when (op) {
            AgentOp.RENAME -> {
                val ok = checkName(name ?: "") as? NameCheck.Ok ?: return Plan.Refused(Refusal.BAD_INPUT)
                AgentDoFrame(agent.agentId, msgId, op, name = ok.name)
            }
            AgentOp.ALLOW -> {
                if (name == null || name.isBlank()) AgentDoFrame(agent.agentId, msgId, op)
                else AgentDoFrame(agent.agentId, msgId, op, name = (checkName(name) as? NameCheck.Ok ?: return Plan.Refused(Refusal.BAD_INPUT)).name)
            }
            AgentOp.BLOCK, AgentOp.UNBLOCK -> AgentDoFrame(agent.agentId, msgId, op)
            AgentOp.MERGE -> {
                // The same key only: `mergeTargets` holds only live agents of the same key and portal. The portal also checks it.
                if (target == null || mergeTargets.none { it.serverId == target.serverId && it.agentId == target.agentId }) {
                    return Plan.Refused(Refusal.BAD_INPUT)
                }
                AgentDoFrame(agent.agentId, msgId, op, into = target.agentId)
            }
        }
        return try {
            frame.validate()
            Plan.Send(frame)
        } catch (e: FrameException) {
            Plan.Refused(Refusal.BAD_INPUT)
        }
    }

    /** The operation of an outbox entry of type `agent_do`, or null. */
    fun frameOf(entry: OutboxEntry): AgentDoFrame? =
        if (entry.type != OutboxType.AGENT_DO) null else runCatching { entry.toFrame() }.getOrNull() as? AgentDoFrame

    /** The actions of [agentId] that wait in the outbox for the result of the portal. */
    fun pendingOps(entries: List<OutboxEntry>, agentId: String): Set<AgentOp> =
        entries.filter { it.isPending && it.requestId == agentId }.mapNotNull { frameOf(it)?.op }.toSet()

    // ------------------------------------------------------------ the words of a result

    /** The line for the snackbar after an `agent_result`. [op] is the action that the owner asked for, if the phone knows it. */
    fun resultLine(status: AgentResultStatus, op: AgentOp?): String = when (status) {
        AgentResultStatus.OK -> when (op) {
            AgentOp.RENAME -> "The name is saved."
            AgentOp.ALLOW -> "The agent is allowed. You can approve its requests now."
            AgentOp.BLOCK -> "The agent is blocked. Its open requests are cancelled."
            AgentOp.UNBLOCK -> "The agent is not blocked now."
            AgentOp.MERGE -> "The agents are merged."
            null -> "Done."
        }
        AgentResultStatus.NOT_FOUND -> "This agent is gone."
        AgentResultStatus.INVALID_NAME -> "The portal does not take this name. Use another name."
        AgentResultStatus.NAME_TAKEN -> "Another agent of this key has this name. Use another name."
        AgentResultStatus.BAD_STATE -> "This cannot be done now. The agent changed. Look at it again."
        AgentResultStatus.CROSS_KEY -> "An agent can be merged only into an agent of the same key."
        AgentResultStatus.NOT_ENABLED -> "This portal does not use agents."
    }

    /** True for a result that did the work, so the screen can close its form. */
    fun succeeded(status: AgentResultStatus): Boolean = status == AgentResultStatus.OK

    /** The words of a result frame with the op of its outbox entry. */
    fun resultLine(f: AgentResultFrame, entry: OutboxEntry?): String = resultLine(f.status, entry?.let { frameOf(it)?.op })
}
