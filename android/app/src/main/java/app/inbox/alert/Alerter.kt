package app.inbox.alert

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.MediaPlayer
import android.media.RingtoneManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.os.VibrationAttributes
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.provider.Settings
import android.util.Log
import app.inbox.R
import app.inbox.data.Alert
import app.inbox.data.Store
import app.inbox.service.Registry
import app.inbox.ui.AlarmActivity
import app.inbox.ui.MainActivity

/**
 * Turns a stored alert into what the subscriber asked for:
 * - `alarm`: full-screen alert, looping alarm sound at full alarm volume through
 *   silent / vibrate / do-not-disturb, vibration, until stopped on the phone;
 * - `ring`: one sound on the alarm stream (audible in silent / vibrate) + vibration;
 * - `notify`: a standard notification that follows the phone sound mode.
 */
object Alerter {
    const val CH_ALARM = "alarm"
    /** Alarm already on screen (phone in use): no heads-up hiding the alarm page. */
    const val CH_ALARM_ACTIVE = "alarm_active"
    const val CH_RING = "ring"
    const val CH_NOTIFY = "notify"
    const val CH_STATUS = "status"
    const val CH_WARNING = "warning"
    private const val TAG = "Inbox.Alerter"

    private val alarmAudio: AudioAttributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_ALARM)
        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
        .build()

    fun createChannels(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java)
        val alarm = NotificationChannel(CH_ALARM, "Alarms", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "Critical alerts: ring until you stop them, even in silent mode"
            setSound(null, null) // played by AlarmPlayer on the alarm stream
            enableVibration(false) // vibration handled by AlarmPlayer
            setBypassDnd(true)
            lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        }
        val active = NotificationChannel(CH_ALARM_ACTIVE, "Alarm on screen", NotificationManager.IMPORTANCE_LOW).apply {
            description = "Ringing alarm already open full screen"
            setSound(null, null)
            enableVibration(false)
            lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        }
        val ring = NotificationChannel(CH_RING, "Rings", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "One sound, even in silent or vibrate mode"
            setSound(null, null)
            enableVibration(false)
            setBypassDnd(true)
            lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        }
        val notify = NotificationChannel(CH_NOTIFY, "Notifications", NotificationManager.IMPORTANCE_DEFAULT).apply {
            description = "Standard notifications, following the phone sound mode"
        }
        val status = NotificationChannel(CH_STATUS, "Connection status", NotificationManager.IMPORTANCE_MIN).apply {
            description = "Permanent notification of the Inbox service"
            setShowBadge(false)
        }
        val warning = NotificationChannel(CH_WARNING, "Connection lost", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "Warns when Inbox can no longer receive alerts"
        }
        nm.createNotificationChannels(listOf(alarm, active, ring, notify, status, warning))
    }

    /** Notification tag/id: unique per alert, so re-posting replaces instead of duplicating. */
    private fun tag(a: Alert) = "alert:${a.serverId}:${a.alertId}"
    private const val ALERT_NOTIFICATION_ID = 1000

    private fun requestCode(a: Alert, salt: Int) = (tag(a).hashCode() * 31) + salt

    fun present(context: Context, a: Alert) {
        // Phone in use and allowed to draw over apps: the alarm page is opened directly,
        // so the notification must not add a heads-up on top of it. Otherwise the
        // full-screen intent (locked / screen off) or its heads-up fallback is used.
        val direct = a.action == "alarm" && phoneInUse(context) && Settings.canDrawOverlays(context)
        post(context, a, direct)
        when (a.action) {
            "alarm" -> {
                AlarmPlayer.start(context, a)
                openAlarmScreen(context, a)
            }
            "ring" -> playOnce(context)
            else -> {}
        }
    }

    /**
     * Once the alarm page is on screen, replaces each ringing alarm's heads-up with
     * the same notification on the discreet channel, so it does not cover the page.
     */
    fun quietAlarmNotifications(context: Context, alerts: List<Alert>) {
        val nm = context.getSystemService(NotificationManager::class.java)
        for (a in alerts) {
            nm.cancel(tag(a), ALERT_NOTIFICATION_ID)
            post(context, a, direct = true)
        }
    }

    private fun post(context: Context, a: Alert, direct: Boolean) {
        val nm = context.getSystemService(NotificationManager::class.java)
        val title = a.description ?: a.groupName
        // The details are Markdown: the notification shows their first plain line.
        val body = a.details?.let { app.inbox.md.Markdown.firstLine(it) } ?: context.getString(R.string.alert_in_group, a.groupName)
        // Tapping the notification opens the alert in the app, which marks it read.
        val open = PendingIntent.getActivity(
            context, requestCode(a, 1),
            MainActivity.openAlert(context, a.serverId, a.alertId),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val channel = when (a.action) {
            "alarm" -> if (direct) CH_ALARM_ACTIVE else CH_ALARM
            "ring" -> CH_RING
            else -> CH_NOTIFY
        }
        val b = Notification.Builder(context, channel)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(Notification.BigTextStyle().bigText(
                a.details?.let { app.inbox.md.Markdown.plainText(it).take(800) }?.ifEmpty { null } ?: body
            ))
            .setWhen(a.createdAtMs)
            .setShowWhen(true)
            .setContentIntent(open)
            .setAutoCancel(a.action != "alarm")
            .setVisibility(Notification.VISIBILITY_PUBLIC)
        if (a.action != "alarm") {
            // Dismissing the notification or tapping "Mark as read" marks it read.
            val read = PendingIntent.getBroadcast(
                context, requestCode(a, 4),
                Intent(context, ReadReceiver::class.java)
                    .putExtra(ReadReceiver.EXTRA_SERVER, a.serverId)
                    .putExtra(ReadReceiver.EXTRA_ALERT, a.alertId),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            b.setDeleteIntent(read)
                .addAction(Notification.Action.Builder(null, context.getString(R.string.alert_mark_read), read).build())
        }
        if (a.action == "alarm") {
            val full = PendingIntent.getActivity(
                context, requestCode(a, 3), AlarmActivity.intent(context, a.serverId, a.alertId),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            val stop = PendingIntent.getBroadcast(
                context, requestCode(a, 2),
                Intent(context, StopReceiver::class.java)
                    .putExtra(StopReceiver.EXTRA_SERVER, a.serverId)
                    .putExtra(StopReceiver.EXTRA_ALERT, a.alertId),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            b.setCategory(Notification.CATEGORY_ALARM)
                .setOngoing(true)
                .addAction(Notification.Action.Builder(null, context.getString(R.string.alert_stop), stop).build())
                .setContentIntent(full)
                .setColor(0xFFB42318.toInt())
            if (!direct) b.setFullScreenIntent(full, true)
        } else {
            b.setCategory(Notification.CATEGORY_MESSAGE)
        }
        try {
            nm.notify(tag(a), ALERT_NOTIFICATION_ID, b.build())
        } catch (e: SecurityException) {
            Log.w(TAG, "notification permission missing", e)
        }
    }

    /**
     * The full-screen intent only launches the alarm page when the phone is locked or
     * the screen is off; while the phone is in use it degrades to a heads-up. With the
     * overlay permission a background activity start is allowed, so the alarm page
     * opens in every case.
     */
    private fun phoneInUse(context: Context): Boolean {
        val pm = context.getSystemService(PowerManager::class.java)
        val km = context.getSystemService(android.app.KeyguardManager::class.java)
        return pm.isInteractive && !km.isKeyguardLocked
    }

    private fun openAlarmScreen(context: Context, a: Alert) {
        if (!Settings.canDrawOverlays(context)) return
        try {
            context.startActivity(
                AlarmActivity.intent(context, a.serverId, a.alertId).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP)
            )
        } catch (e: Exception) {
            Log.w(TAG, "cannot open the alarm screen", e)
        }
    }

    /**
     * Marks an alert read on this phone: its notification disappears and, for an
     * alarm still ringing, the sound stops. Local only: nothing is sent.
     */
    fun markRead(context: Context, serverId: Long, alertId: String) {
        val store = Store.get(context)
        store.markRead(serverId, alertId)
        val a = store.alert(serverId, alertId) ?: return Registry.changed()
        if (a.action == "alarm" && a.stoppedAtMs == null) {
            stopAlarm(context, serverId, alertId)
        } else {
            context.getSystemService(NotificationManager::class.java).cancel(tag(a), ALERT_NOTIFICATION_ID)
            Registry.changed()
        }
    }

    /** "Mark all as read". */
    fun markAllRead(context: Context) {
        for (a in Store.get(context).unreadAlerts()) markRead(context, a.serverId, a.alertId)
        Registry.changed()
    }

    /** Before a server is deleted: silences its alarms and removes its notifications. */
    fun forgetServer(context: Context, serverId: Long) {
        val nm = context.getSystemService(NotificationManager::class.java)
        for (a in Store.get(context).serverAlerts(serverId)) {
            if (a.action == "alarm" && a.stoppedAtMs == null) AlarmPlayer.stop(context, serverId, a.alertId)
            nm.cancel(tag(a), ALERT_NOTIFICATION_ID)
        }
        nm.cancel(app.inbox.service.InboxService.WARNING_BASE_ID + serverId.toInt())
    }

    /** Stops every alarm still ringing on this phone (local only). */
    fun stopAllAlarms(context: Context) {
        for (a in Store.get(context).ringingAlarms()) stopAlarm(context, a.serverId, a.alertId)
    }

    /**
     * Stops an alarm on this phone: sound, vibration and the ongoing notification.
     * Purely local: alerts are one-way and nothing is sent to the server.
     */
    fun stopAlarm(context: Context, serverId: Long, alertId: String) {
        val store = Store.get(context)
        store.markStopped(serverId, alertId)
        // Stopping an alarm is seeing it: it is read too.
        store.markRead(serverId, alertId)
        val a = store.alert(serverId, alertId)
        if (a != null) {
            context.getSystemService(NotificationManager::class.java).cancel(tag(a), ALERT_NOTIFICATION_ID)
        }
        AlarmPlayer.stop(context, serverId, alertId)
        Registry.changed()
    }

    // ------------------------------------------------------------ ring

    private fun playOnce(context: Context) {
        val audio = context.getSystemService(AudioManager::class.java)
        val max = audio.getStreamMaxVolume(AudioManager.STREAM_ALARM)
        val previous = audio.getStreamVolume(AudioManager.STREAM_ALARM)
        val target = maxOf(previous, (max * 0.7).toInt())
        try {
            audio.setStreamVolume(AudioManager.STREAM_ALARM, target, 0)
        } catch (e: SecurityException) {
            Log.w(TAG, "cannot raise alarm volume", e)
        }
        val uri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
        try {
            MediaPlayer().apply {
                setAudioAttributes(alarmAudio)
                setDataSource(context, uri)
                setOnCompletionListener {
                    it.release()
                    try {
                        audio.setStreamVolume(AudioManager.STREAM_ALARM, previous, 0)
                    } catch (_: SecurityException) {
                    }
                }
                prepare()
                start()
            }
        } catch (e: Exception) {
            Log.e(TAG, "ring playback failed", e)
        }
        vibrate(context, VibrationEffect.createWaveform(longArrayOf(0, 400, 200, 400), -1))
    }

    /** Vibrates with alarm attributes, so it is not muted by the ringer mode. */
    internal fun vibrate(context: Context, effect: VibrationEffect) {
        val v = vibrator(context)
        if (Build.VERSION.SDK_INT >= 33) {
            v.vibrate(effect, VibrationAttributes.createForUsage(VibrationAttributes.USAGE_ALARM))
        } else {
            @Suppress("DEPRECATION")
            v.vibrate(effect, alarmAudio)
        }
    }

    internal fun vibrator(context: Context): Vibrator =
        if (Build.VERSION.SDK_INT >= 31) {
            context.getSystemService(VibratorManager::class.java).defaultVibrator
        } else {
            @Suppress("DEPRECATION")
            context.getSystemService(Vibrator::class.java)
        }

    internal fun alarmUri(): Uri =
        RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION)

    internal val alarmAttributes get() = alarmAudio
}

/**
 * The looping alarm. Keeps ringing while at least one alarm has not been stopped.
 */
object AlarmPlayer {
    private const val TAG = "Inbox.AlarmPlayer"
    private val active = linkedSetOf<String>()
    private var player: MediaPlayer? = null
    private var savedVolume: Int? = null
    private var wakeLock: PowerManager.WakeLock? = null

    private fun key(serverId: Long, alertId: String) = "$serverId:$alertId"

    @Synchronized
    fun start(context: Context, a: Alert) {
        active.add(key(a.serverId, a.alertId))
        if (player != null) return
        val app = context.applicationContext
        val audio = app.getSystemService(AudioManager::class.java)
        // Leave silent / vibrate: restore the normal ringer mode when allowed to.
        val nm = app.getSystemService(NotificationManager::class.java)
        if (audio.ringerMode != AudioManager.RINGER_MODE_NORMAL && nm.isNotificationPolicyAccessGranted) {
            try {
                audio.ringerMode = AudioManager.RINGER_MODE_NORMAL
            } catch (e: SecurityException) {
                Log.w(TAG, "cannot change ringer mode", e)
            }
        }
        try {
            savedVolume = audio.getStreamVolume(AudioManager.STREAM_ALARM)
            audio.setStreamVolume(AudioManager.STREAM_ALARM, audio.getStreamMaxVolume(AudioManager.STREAM_ALARM), 0)
        } catch (e: SecurityException) {
            Log.w(TAG, "cannot raise alarm volume", e)
        }
        wakeLock = app.getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "inbox:alarm")
            .apply { acquire(30 * 60 * 1000L) }
        player = try {
            MediaPlayer().apply {
                setAudioAttributes(Alerter.alarmAttributes)
                setDataSource(app, Alerter.alarmUri())
                isLooping = true
                prepare()
                start()
            }
        } catch (e: Exception) {
            Log.e(TAG, "alarm playback failed", e)
            null
        }
        Alerter.vibrate(app, VibrationEffect.createWaveform(longArrayOf(0, 800, 400, 800, 400), 0))
    }

    @Synchronized
    fun stop(context: Context, serverId: Long, alertId: String) {
        active.remove(key(serverId, alertId))
        if (active.isNotEmpty()) return
        val app = context.applicationContext
        player?.let {
            try {
                it.stop()
            } catch (_: IllegalStateException) {
            }
            it.release()
        }
        player = null
        Alerter.vibrator(app).cancel()
        savedVolume?.let {
            try {
                app.getSystemService(AudioManager::class.java).setStreamVolume(AudioManager.STREAM_ALARM, it, 0)
            } catch (_: SecurityException) {
            }
        }
        savedVolume = null
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
    }

    @Synchronized
    fun isRinging(): Boolean = active.isNotEmpty()
}

/** "Mark as read" action, and dismissal, of ring / notify notifications (local only). */
class ReadReceiver : BroadcastReceiver() {
    companion object {
        const val EXTRA_SERVER = "server_id"
        const val EXTRA_ALERT = "alert_id"
    }

    override fun onReceive(context: Context, intent: Intent) {
        val serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val alertId = intent.getStringExtra(EXTRA_ALERT) ?: return
        if (serverId < 0) return
        Alerter.markRead(context, serverId, alertId)
    }
}

/** "Stop alarm" action on alarm notifications (local only). */
class StopReceiver : BroadcastReceiver() {
    companion object {
        const val EXTRA_SERVER = "server_id"
        const val EXTRA_ALERT = "alert_id"
    }

    override fun onReceive(context: Context, intent: Intent) {
        val serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val alertId = intent.getStringExtra(EXTRA_ALERT) ?: return
        if (serverId < 0) return
        Alerter.stopAlarm(context, serverId, alertId)
    }
}
