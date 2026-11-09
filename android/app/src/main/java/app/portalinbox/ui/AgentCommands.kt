package app.portalinbox.ui

import android.content.Context
import app.portalinbox.data.AgentActions
import app.portalinbox.data.OutboxAnswer
import app.portalinbox.data.Submit
import app.portalinbox.proto.AgentOp
import app.portalinbox.service.Items
import app.portalinbox.service.Registry

/**
 * Sends an action of the owner on an agent. The engine puts the frame in the outbox, the connection sends it, and the result comes
 * back as an `agent_result` (a line for the snackbar: see `AgentNotices`). No biometric check: an action on an agent never runs a
 * command. Only an approval needs it.
 */
object AgentCommands {
    /** Returns the words of a refusal, or null when the action waits in the outbox. */
    fun run(context: Context, serverId: Long, agentId: String, op: AgentOp, name: String? = null, into: String? = null): String? {
        val submit = Items.engine(context).agentDo(serverId, agentId, op, name, into)
        if (submit is Submit.Refused) return OutboxAnswer.submitLine(submit)
        Registry.connections[serverId]?.flushOutbox()
        Registry.changed()
        return null
    }

    /** The actions that wait in the outbox for [agentId]. */
    fun pending(context: Context, serverId: Long, agentId: String): Set<AgentOp> =
        AgentActions.pendingOps(Items.repository(context).pendingOutbox(serverId), agentId)
}
