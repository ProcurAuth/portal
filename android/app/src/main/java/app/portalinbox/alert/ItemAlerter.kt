package app.portalinbox.alert

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEvent
import app.portalinbox.service.Items
import app.portalinbox.service.Registry
import app.portalinbox.ui.OpenScreens

/**
 * The entry of the item events into the one notification. It posts nothing by itself: each event refreshes the aggregate
 * of [AttentionNotifier], and the aggregate decides what the one notification says and if it alerts again.
 *
 * - A new request, a request that comes back ("The agent is back") and a change of a request: the aggregate sees the new thing.
 * - A turn of an agent: one more reply, unless the owner reads that discussion now.
 * - A final item: it leaves the aggregate. The notification goes away when nothing waits.
 * - A digest: the same notification gets fresh ages. It never makes a second notification.
 */
object ItemAlerter {
    fun onEvent(context: Context, event: ItemEvent) {
        when (event) {
            is ItemEvent.AgentMessage -> {
                // The owner reads this thread now: the turn is on the screen, a notification would only repeat it.
                if (OpenScreens.isThread(event.serverId, event.requestId)) return
                AttentionNotifier.onAgentMessage(context, event.serverId, event.requestId)
            }
            is ItemEvent.UserMessage -> AttentionNotifier.clearReply(context, event.serverId, event.requestId)
            else -> AttentionNotifier.refresh(context)
        }
    }

    /** Shows what the process died before it showed. Called when the service starts. */
    fun recover(context: Context) = AttentionNotifier.refresh(context)

    /** The owner opens a discussion: its reply leaves the notification. */
    fun withdrawReply(context: Context, serverId: Long, requestId: String) = AttentionNotifier.clearReply(context, serverId, requestId)

    /** The owner just answered on the screen: the item does not wait for the owner any more. */
    fun withdrawNow(context: Context, item: Item) {
        AttentionNotifier.clearReply(context, item.serverId, item.requestId)
        AttentionNotifier.refresh(context)
    }

    fun forgetServer(context: Context, serverId: Long) = AttentionNotifier.forget(context, serverId)
}

/**
 * The action Decline of the notification. It exists only when the notification holds exactly one approval and nothing else. A
 * decline needs no gate: it never runs the command. There is no receiver for an approval: Approve is never an action of a
 * notification.
 */
class DeclineReceiver : BroadcastReceiver() {
    companion object {
        const val EXTRA_SERVER = "server_id"
        const val EXTRA_REQUEST = "request_id"
    }

    override fun onReceive(context: Context, intent: Intent) {
        val serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val requestId = intent.getStringExtra(EXTRA_REQUEST) ?: return
        if (serverId < 0) return
        Items.engine(context).decline(serverId, requestId)
        Registry.connections[serverId]?.flushOutbox()
        AttentionNotifier.refresh(context)
        Registry.changed()
    }
}
