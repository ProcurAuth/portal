package app.portalinbox.alert

import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.util.Log
import app.portalinbox.R
import app.portalinbox.data.ItemEvent
import app.portalinbox.data.OutboxType
import app.portalinbox.portal.PortalIdentity
import app.portalinbox.portal.PortalPalette
import app.portalinbox.data.PrefsSync
import app.portalinbox.data.Store
import app.portalinbox.service.Items
import app.portalinbox.service.PendingPrefsStore
import app.portalinbox.service.Registry
import app.portalinbox.service.ServerClock
import app.portalinbox.ui.AlarmActivity
import app.portalinbox.ui.MainActivity

/**
 * The Android side of the one notification. It reads the state of the phone, runs [AttentionCenter], and builds the
 * `Notification`. All decisions are in [Attention] (pure, tested). Items, alerts, replies, the digest and the warnings of the
 * portals all come here: nothing else posts a notification of them.
 *
 * Rules (decision "one notification"):
 * - One id and one tag. The notification is updated in place.
 * - The version of a locked screen has the counts and the nickname of a portal (unless hidden). It never has a body.
 * - Tapping it opens the home list. While an alarm rings, it opens the alarm page.
 * - It has the action Decline only when it holds exactly one approval and nothing else. Never Approve.
 */
object AttentionNotifier {
    private const val TAG = "Inbox.Attention"
    private const val RED = 0xFFB42318.toInt()

    private val replies = LinkedHashSet<Pair<Long, String>>()
    private val digests = DigestPlan.Memory()
    @Volatile private var alarmDirect = false
    @Volatile private var center: AttentionCenter? = null

    private fun center(context: Context): AttentionCenter =
        center ?: synchronized(this) { center ?: AttentionCenter(AndroidSink(context.applicationContext)).also { center = it } }

    /** The alarm page is on screen (or opens by itself): the notification then has no heads-up and no full-screen intent. */
    fun alarmDirect(direct: Boolean) {
        alarmDirect = direct
    }

    /**
     * Brings the notification up to date. [force] is true after an event. A periodic check passes false. [excludeServer] is a
     * portal that is being removed: it does not count.
     */
    fun refresh(context: Context, force: Boolean = true, excludeServer: Long? = null) {
        val input = gather(context.applicationContext, excludeServer)
        center(context).update(input, force, alarmDirect)
        // The phone has shown what it holds: a restart does not show it as new.
        val repo = Items.repository(context)
        for (i in input.items) if (!i.notified) repo.markNotified(i.serverId, i.requestId)
        // A new agent counts once: a restart of the process never alerts again for an agent that the notification showed.
        for (n in input.agents) if (!n.notified) repo.markAgentNotified(n.serverId, n.agentId)
        val store = Store.get(context)
        for (a in input.alerts) if (!a.notified) store.markNotified(a.serverId, a.alertId)
    }

    /** A turn of an agent in a discussion: one more reply in the notification, until the owner opens the discussion. */
    fun onAgentMessage(context: Context, serverId: Long, requestId: String) {
        synchronized(replies) { replies.add(serverId to requestId) }
        refresh(context)
    }

    /** The owner opened the discussion, answered, or the item is gone. */
    fun clearReply(context: Context, serverId: Long, requestId: String) {
        val had = synchronized(replies) { replies.remove(serverId to requestId) }
        if (had) refresh(context)
    }

    /**
     * A `digest` frame of a portal. The phone keeps it, and the one notification refreshes with a "N waiting" part for that portal.
     * It makes no notification of its own and no sound. The memory lives as long as the process: the notification that is on
     * screen stays, and the next digest of the portal brings the part back.
     */
    fun onDigest(context: Context, e: ItemEvent.Digest) {
        val frame = DigestPlan.Frame(e.serverId, e.deliveryId, e.count, e.oldestCreatedAtMs, e.ids, System.currentTimeMillis())
        if (digests.record(frame)) refresh(context)
    }

    fun forget(context: Context, serverId: Long) {
        digests.forget(serverId)
        synchronized(replies) { replies.removeAll { it.first == serverId } }
        refresh(context, excludeServer = serverId)
    }

    private fun gather(context: Context, excludeServer: Long?): Attention.Input {
        val store = Store.get(context)
        val repo = Items.repository(context)
        val pending = PendingPrefsStore.get(context)
        val phoneNow = System.currentTimeMillis()
        val servers = store.servers().filter { it.id != excludeServer }
        val portals = servers.map { s ->
            val st = Registry.status(s.id)
            Attention.Portal(
                identity = PortalIdentity.of(s),
                hideNameOnLock = s.hideNameOnLock,
                muted = s.mutedUntilMs?.let { it > phoneNow } == true,
                quiet = PrefsSync.quiet(repo.prefs(s.id)?.prefs, pending.get(s.id)),
                nowMs = ServerClock.now(context, s.id),
                down = Attention.downOf(st.state, st.sinceMs, s.offlineWarningMs, phoneNow),
            )
        }
        // An item that the owner answered waits in the outbox for the server: it does not wait for the owner any more.
        val answered = servers.associate { s -> s.id to repo.pendingOutbox(s.id).filter { it.type == OutboxType.ANSWER }.map { it.requestId }.toSet() }
        val items = servers.flatMap { s -> repo.openItems(s.id).filter { it.requestId !in answered.getValue(s.id) } }
        val ids = servers.map { it.id }.toSet()
        val alerts = store.unreadAlerts().filter { it.serverId in ids }
        val pendingReplies = synchronized(replies) { replies.toList() }.mapNotNull { (serverId, requestId) ->
            if (serverId !in ids) return@mapNotNull null
            repo.item(serverId, requestId)?.let { Attention.Reply(serverId, requestId, it.request.urgency) }
        }
        // The digest: an id that the phone knows as closed (or answered) leaves the count of its portal. The open items are at hand.
        val openById = items.associateBy { it.serverId to it.requestId }
        val waiting = DigestPlan.merge(
            digests.frames().filter { it.serverId in ids },
            { f ->
                f.ids.mapNotNull { id ->
                    val item = openById[f.serverId to id] ?: repo.item(f.serverId, id) ?: return@mapNotNull null
                    id to DigestPlan.Known(item.isOpen && id !in answered.getValue(f.serverId), item.request.createdAtMs)
                }.toMap()
            },
            phoneNow,
        )
        // The agents that wait for the owner: state `new`, never a bucket. Only the name of the key goes in: it is text of the portal.
        val agents = repo.allAgents().filter { it.serverId in ids && it.isNew && !it.isBucket }
            .map { Attention.NewAgent(it.serverId, it.agentId, it.keyName, it.notified) }
        return Attention.Input(portals, items, alerts, pendingReplies, phoneNow, network = hasNetwork(context), digests = waiting, agents = agents)
    }

    /** True when the phone has a network. With no network the status notification says it once, and no portal warns. */
    fun hasNetwork(context: Context): Boolean {
        val cm = context.getSystemService(android.net.ConnectivityManager::class.java)
        return cm.activeNetwork != null
    }

    private class AndroidSink(private val context: Context) : AttentionCenter.Sink {
        private val nm = context.getSystemService(NotificationManager::class.java)

        override fun isShown(): Boolean = nm.activeNotifications.any { it.id == Attention.ID && it.tag == Attention.TAG }

        override fun cancel() = nm.cancel(Attention.TAG, Attention.ID)

        override fun play(sound: Attention.Sound) {
            when (sound) {
                Attention.Sound.RING -> Alerter.playOnce(context)
                Attention.Sound.PHONE -> playPhone()
                else -> {}
            }
        }

        /** The sound of a notification, on the notification stream: the silent and vibrate modes of the phone decide. */
        private fun playPhone() {
            try {
                val uri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION)
                RingtoneManager.getRingtone(context, uri)?.apply {
                    audioAttributes = AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_NOTIFICATION)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .build()
                    play()
                }
            } catch (e: Exception) {
                Log.w(TAG, "notification sound failed", e)
            }
        }

        private fun intent(code: Int, i: Intent) =
            PendingIntent.getActivity(context, code, i, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)

        override fun post(plan: Attention.Plan) {
            val open = when (val t = plan.target) {
                Attention.Target.Home -> intent(1, MainActivity.home(context))
                is Attention.Target.Alarm -> intent(1, AlarmActivity.intent(context, t.serverId, t.alertId))
            }
            val c = plan.content
            val public = Notification.Builder(context, plan.channel)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(c.publicTitle)
                .setContentText(c.publicText)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .build()
            val b = Notification.Builder(context, plan.channel)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(c.title)
                .setContentText(c.text)
                .setContentIntent(open)
                .setShowWhen(false)
                .setAutoCancel(!plan.alarm)
                .setOngoing(plan.alarm)
                .setOnlyAlertOnce(!plan.alert)
                .setCategory(if (plan.alarm) Notification.CATEGORY_ALARM else Notification.CATEGORY_MESSAGE)
                .setVisibility(Notification.VISIBILITY_PRIVATE)
                .setPublicVersion(public)
            if (c.lines.isNotEmpty()) {
                b.setStyle(Notification.InboxStyle().also { s -> c.lines.forEach { s.addLine(it) } })
            }
            val night = (context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES
            when {
                plan.alarm -> b.setColor(RED)
                plan.colorIndex != null -> b.setColor(PortalPalette.color(plan.colorIndex, night))
            }
            plan.decline?.let { (serverId, requestId) ->
                val decline = PendingIntent.getBroadcast(
                    context, 2,
                    Intent(context, DeclineReceiver::class.java)
                        .setAction("app.portalinbox.DECLINE:$serverId:$requestId")
                        .putExtra(DeclineReceiver.EXTRA_SERVER, serverId)
                        .putExtra(DeclineReceiver.EXTRA_REQUEST, requestId),
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                )
                b.addAction(Notification.Action.Builder(null, context.getString(R.string.notification_decline), decline).build())
            }
            if (plan.stopAlarm) {
                val stop = PendingIntent.getBroadcast(
                    context, 4, Intent(context, StopReceiver::class.java),
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                )
                b.addAction(Notification.Action.Builder(null, context.getString(R.string.alert_stop), stop).build())
            }
            val t = plan.target
            if (plan.fullScreen && t is Attention.Target.Alarm) {
                b.setFullScreenIntent(intent(3, AlarmActivity.intent(context, t.serverId, t.alertId)), true)
            }
            try {
                nm.notify(plan.tag, plan.id, b.build())
            } catch (e: SecurityException) {
                Log.w(TAG, "notification permission missing", e)
            }
        }
    }
}
