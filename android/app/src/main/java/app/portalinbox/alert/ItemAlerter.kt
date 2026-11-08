package app.portalinbox.alert

import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import app.portalinbox.R
import app.portalinbox.data.Item
import app.portalinbox.data.ItemEvent
import app.portalinbox.service.Items
import app.portalinbox.service.Registry
import app.portalinbox.ui.ApprovalActivity
import app.portalinbox.ui.MainActivity

/**
 * The notifications of the items. The texts and the decisions are in [NotificationText] and [NotificationPlan] (pure, tested).
 * This object only builds the Android objects.
 *
 * Rules (CONF 5.3):
 * - The text names the code, the key and the service. It never holds the command or the explanation.
 * - The notification is private: a locked screen shows the public version only.
 * - The action is Decline. There is no Approve action: an approval always goes through the screen, the read gate and the
 *   biometric prompt.
 * - The tag is the chain id. A settled item withdraws its notification, unless a newer request of the same chain holds it.
 */
object ItemAlerter {
    private const val TAG = "Inbox.ItemAlerter"
    const val ITEM_NOTIFICATION_ID = 3000

    fun onEvent(context: Context, event: ItemEvent) {
        val plan = NotificationPlan.forEvent(event)
        if (plan != null) {
            post(context, plan)
            val item = when (event) {
                is ItemEvent.NewRequest -> event.item
                is ItemEvent.Revived -> event.item
                is ItemEvent.Updated -> event.item
                else -> null
            }
            if (item != null && !item.notified) Items.repository(context).markNotified(item.serverId, item.requestId)
            return
        }
        if (event is ItemEvent.Settled) withdraw(context, event.item)
    }

    /** Notifies the open items that the process died before it showed. Called when the service starts. */
    fun recover(context: Context) {
        val repo = Items.repository(context)
        val store = app.portalinbox.data.Store.get(context)
        for (server in store.servers()) {
            for (item in repo.openItems(server.id)) {
                if (item.notified) continue
                NotificationPlan.forEvent(ItemEvent.NewRequest(item))?.let { post(context, it) }
                repo.markNotified(item.serverId, item.requestId)
            }
        }
    }

    /** Removes the notification of a final item, unless a newer request of its chain took the tag over. */
    fun withdraw(context: Context, settled: Item) {
        val open = Items.repository(context).openItems(settled.serverId)
        val tag = NotificationPlan.withdrawTag(settled, open) ?: return
        context.getSystemService(NotificationManager::class.java).cancel(tag, ITEM_NOTIFICATION_ID)
    }

    /** Removes the notification of an item that the owner just answered on the screen. */
    fun withdrawNow(context: Context, item: Item) {
        context.getSystemService(NotificationManager::class.java).cancel(NotificationPlan.tag(item), ITEM_NOTIFICATION_ID)
    }

    fun forgetServer(context: Context, serverId: Long) {
        val nm = context.getSystemService(NotificationManager::class.java)
        for (item in Items.repository(context).items(serverId)) nm.cancel(NotificationPlan.tag(item), ITEM_NOTIFICATION_ID)
    }

    private fun requestCode(plan: NotificationPlan.Plan, salt: Int) = (plan.tag.hashCode() * 31 + plan.requestId.hashCode()) * 31 + salt

    private fun post(context: Context, plan: NotificationPlan.Plan) {
        val nm = context.getSystemService(NotificationManager::class.java)
        val open = PendingIntent.getActivity(
            context, requestCode(plan, 1),
            if (plan.declineAction) ApprovalActivity.intent(context, plan.serverId, plan.requestId) else MainActivity.home(context),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        // The version of a locked screen: the code only.
        val public = Notification.Builder(context, plan.channel)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(plan.content.publicTitle)
            .setContentText(plan.content.publicText)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .build()
        val b = Notification.Builder(context, plan.channel)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(plan.content.title)
            .setContentText(plan.content.text)
            .setContentIntent(open)
            .setAutoCancel(true)
            .setOnlyAlertOnce(plan.refresh)
            .setCategory(Notification.CATEGORY_MESSAGE)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .setPublicVersion(public)
        if (plan.declineAction) {
            val decline = PendingIntent.getBroadcast(
                context, requestCode(plan, 2),
                Intent(context, DeclineReceiver::class.java)
                    .setAction("app.portalinbox.DECLINE:${plan.serverId}:${plan.requestId}")
                    .putExtra(DeclineReceiver.EXTRA_SERVER, plan.serverId)
                    .putExtra(DeclineReceiver.EXTRA_REQUEST, plan.requestId),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            b.addAction(Notification.Action.Builder(null, context.getString(R.string.notification_decline), decline).build())
        } else {
            b.addAction(Notification.Action.Builder(null, context.getString(R.string.notification_open), open).build())
        }
        try {
            nm.notify(plan.tag, ITEM_NOTIFICATION_ID, b.build())
        } catch (e: SecurityException) {
            Log.w(TAG, "notification permission missing", e)
            return
        }
        if (plan.ring) Alerter.playOnce(context)
    }
}

/**
 * The action Decline of a notification. A decline needs no gate: it never runs the command. There is no receiver for an
 * approval: Approve is never an action of a notification.
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
        val repo = Items.repository(context)
        val item = repo.item(serverId, requestId)
        Items.engine(context).decline(serverId, requestId)
        Registry.connections[serverId]?.flushOutbox()
        if (item != null) ItemAlerter.withdrawNow(context, item)
        Registry.changed()
    }
}
