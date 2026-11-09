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
import app.portalinbox.data.PrefsSync
import app.portalinbox.proto.QuietPrefs
import app.portalinbox.service.Items
import app.portalinbox.service.PendingPrefsStore
import app.portalinbox.service.Registry
import app.portalinbox.ui.ApprovalActivity
import app.portalinbox.ui.MainActivity
import app.portalinbox.ui.OpenScreens
import app.portalinbox.ui.ThreadActivity

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
    const val ITEM_NOTIFICATION_ID = NotificationPlan.ID_REQUEST

    /** The quiet hours that apply on this phone for a server: the pending edit, else the last `welcome`. */
    private fun env(context: Context, serverId: Long): NotificationPlan.Env {
        val quiet: QuietPrefs? = PrefsSync.quiet(
            Items.repository(context).prefs(serverId)?.prefs, PendingPrefsStore.get(context).get(serverId),
        )
        return NotificationPlan.Env(quiet, System.currentTimeMillis())
    }

    private fun serverIdOf(event: ItemEvent): Long = when (event) {
        is ItemEvent.NewRequest -> event.item.serverId
        is ItemEvent.Revived -> event.item.serverId
        is ItemEvent.Updated -> event.item.serverId
        is ItemEvent.AgentMessage -> event.serverId
        is ItemEvent.UserMessage -> event.serverId
        is ItemEvent.Settled -> event.item.serverId
        is ItemEvent.Digest -> event.serverId
        is ItemEvent.AnswerResult -> event.serverId
    }

    fun onEvent(context: Context, event: ItemEvent) {
        if (event is ItemEvent.Digest) {
            postDigest(context, event)
            return
        }
        if (event is ItemEvent.AgentMessage) {
            // The owner reads this thread now: the turn is on the screen, a notification would only repeat it.
            if (OpenScreens.isThread(event.serverId, event.requestId)) return
            val item = Items.repository(context).item(event.serverId, event.requestId)
            NotificationPlan.forMessage(event, item, env(context, event.serverId))?.let { post(context, it) }
            return
        }
        val plan = NotificationPlan.forEvent(event, env(context, serverIdOf(event)))
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
                NotificationPlan.forEvent(ItemEvent.NewRequest(item), env(context, item.serverId))?.let { post(context, it) }
                repo.markNotified(item.serverId, item.requestId)
            }
        }
    }

    /** Removes the notification of a final item, unless a newer request of its chain took the tag over. */
    fun withdraw(context: Context, settled: Item) {
        val open = Items.repository(context).openItems(settled.serverId)
        val nm = context.getSystemService(NotificationManager::class.java)
        // A settled item has no discussion: its reply notification goes too.
        nm.cancel(settled.requestId, NotificationPlan.ID_REPLY)
        val tag = NotificationPlan.withdrawTag(settled, open) ?: return
        nm.cancel(tag, ITEM_NOTIFICATION_ID)
    }

    /** Removes the reply notification of an item that the owner opens. */
    fun withdrawReply(context: Context, serverId: Long, requestId: String) {
        context.getSystemService(NotificationManager::class.java).cancel(requestId, NotificationPlan.ID_REPLY)
    }

    /** Removes the notification of an item that the owner just answered on the screen. */
    fun withdrawNow(context: Context, item: Item) {
        val nm = context.getSystemService(NotificationManager::class.java)
        nm.cancel(NotificationPlan.tag(item), ITEM_NOTIFICATION_ID)
        nm.cancel(item.requestId, NotificationPlan.ID_REPLY)
    }

    fun forgetServer(context: Context, serverId: Long) {
        val nm = context.getSystemService(NotificationManager::class.java)
        for (item in Items.repository(context).items(serverId)) {
            nm.cancel(NotificationPlan.tag(item), ITEM_NOTIFICATION_ID)
            nm.cancel(item.requestId, NotificationPlan.ID_REPLY)
        }
        nm.cancel(digestTag(serverId), NotificationPlan.ID_DIGEST)
    }

    private fun digestTag(serverId: Long) = "digest:$serverId"

    /**
     * The summary of the open items (DIAL 4.6). Silent channel. The text has numbers and an age, never a title or a body.
     * Tapping it opens the home list. A digest that the phone knows to be out of date makes no notification.
     */
    private fun postDigest(context: Context, e: ItemEvent.Digest) {
        val repo = Items.repository(context)
        val known = e.ids.mapNotNull { id ->
            repo.item(e.serverId, id)?.let { id to DigestText.Known(it.kind, it.isOpen, it.request.createdAtMs) }
        }.toMap()
        val now = app.portalinbox.service.ServerClock.now(context, e.serverId)
        val content = DigestText.of(e.count, e.oldestCreatedAtMs, e.ids, known, now) ?: return
        val nm = context.getSystemService(NotificationManager::class.java)
        val open = PendingIntent.getActivity(
            context, digestTag(e.serverId).hashCode(), MainActivity.home(context),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val public = Notification.Builder(context, NotificationPlan.CH_DIGEST)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(content.publicTitle)
            .setContentText(content.publicText)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .build()
        val b = Notification.Builder(context, NotificationPlan.CH_DIGEST)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(content.title)
            .setContentText(content.text)
            .setContentIntent(open)
            .setAutoCancel(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_STATUS)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .setPublicVersion(public)
        try {
            nm.notify(digestTag(e.serverId), NotificationPlan.ID_DIGEST, b.build())
        } catch (ex: SecurityException) {
            Log.w(TAG, "notification permission missing", ex)
        }
    }

    private fun requestCode(plan: NotificationPlan.Plan, salt: Int) = (plan.tag.hashCode() * 31 + plan.requestId.hashCode()) * 31 + salt

    private fun post(context: Context, plan: NotificationPlan.Plan) {
        val nm = context.getSystemService(NotificationManager::class.java)
        val target = when (plan.target) {
            NotificationPlan.Target.APPROVAL -> ApprovalActivity.intent(context, plan.serverId, plan.requestId)
            NotificationPlan.Target.THREAD -> ThreadActivity.intent(context, plan.serverId, plan.requestId)
            NotificationPlan.Target.HOME -> MainActivity.home(context)
        }
        val open = PendingIntent.getActivity(
            context, requestCode(plan, 1) + plan.notificationId, target,
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
        } else if (plan.openAction) {
            b.addAction(Notification.Action.Builder(null, context.getString(R.string.notification_open), open).build())
        }
        try {
            nm.notify(plan.tag, plan.notificationId, b.build())
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
