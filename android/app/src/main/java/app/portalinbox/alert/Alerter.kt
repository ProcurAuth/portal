package app.portalinbox.alert

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
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
import app.portalinbox.R
import app.portalinbox.data.Alert
import app.portalinbox.data.Levels
import app.portalinbox.data.Store
import app.portalinbox.service.Registry
import app.portalinbox.service.ServerClock
import app.portalinbox.ui.AlarmActivity

/**
 * Turns a stored alert into what the subscriber asked for. The notification is the one of [AttentionNotifier]: an alert never
 * posts a notification of its own.
 * - `alarm`: full-screen alert, looping alarm sound at full alarm volume through silent / vibrate / do-not-disturb, vibration,
 *   until stopped on the phone, which then gets its silent / vibrate mode and alarm volume back;
 * - `ring`: one sound on the alarm stream (audible in silent / vibrate) + vibration;
 * - `notify`: the sound of a notification, which follows the phone sound mode.
 * The sound of `ring` and `notify` is played by the one notification, when the alert is new and at least as urgent as what the
 * notification already shows. The alarm always rings.
 */
object Alerter {
    const val CH_STATUS = "status"
    private const val TAG = "Inbox.Alerter"

    /** The channels of the first versions. They are removed: the one notification uses [Attention.CH_ATTENTION] and [Attention.CH_QUIET]. */
    private val OLD_CHANNELS = listOf(
        "alarm", "alarm_active", "ring", "notify", "warning", "approvals", "approvals_quiet", "digest", "replies", "replies_quiet",
    )

    private val alarmAudio: AudioAttributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_ALARM)
        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
        .build()

    /**
     * Three channels. The status notification of the service is silent and low. The one notification of items and alerts uses
     * a channel that can alert and a quiet one, and the code picks the channel when it posts (Android cannot silence one
     * notification below API 34). Neither of them has a sound of its own: the code plays the sound that fits the thing that
     * arrived (alarm stream for a ring and an item, notification stream for a plain alert).
     */
    fun createChannels(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java)
        for (id in OLD_CHANNELS) nm.deleteNotificationChannel(id)
        val status = NotificationChannel(CH_STATUS, context.getString(R.string.ch_status_name), NotificationManager.IMPORTANCE_MIN).apply {
            description = context.getString(R.string.ch_status_desc)
            setShowBadge(false)
        }
        val attention = NotificationChannel(Attention.CH_ATTENTION, context.getString(R.string.ch_attention_name), NotificationManager.IMPORTANCE_HIGH).apply {
            description = context.getString(R.string.ch_attention_desc)
            setSound(null, null) // played by the code, on the stream that fits
            enableVibration(false)
            setBypassDnd(true)
            lockscreenVisibility = Notification.VISIBILITY_PRIVATE
        }
        val quiet = NotificationChannel(Attention.CH_QUIET, context.getString(R.string.ch_attention_quiet_name), NotificationManager.IMPORTANCE_LOW).apply {
            description = context.getString(R.string.ch_attention_quiet_desc)
            setSound(null, null)
            enableVibration(false)
            lockscreenVisibility = Notification.VISIBILITY_PRIVATE
        }
        nm.createNotificationChannels(listOf(status, attention, quiet))
    }

    fun present(context: Context, a: Alert) {
        // An alert to the account that the user turned off: it stays in the list, with no sound
        // and no notification.
        if (a.action == Levels.OFF) return
        // Phone in use and allowed to draw over apps: the alarm page is opened directly,
        // so the notification must not add a heads-up on top of it. Otherwise the
        // full-screen intent (locked / screen off) or its heads-up fallback is used.
        if (a.action == "alarm") AttentionNotifier.alarmDirect(phoneInUse(context) && Settings.canDrawOverlays(context))
        AttentionNotifier.refresh(context)
        if (a.action == "alarm") {
            AlarmPlayer.start(context, a)
            openAlarmScreen(context, a)
        }
    }

    /**
     * Once the alarm page is on screen, the one notification stops its heads-up and its full-screen intent, so it does not
     * cover the page.
     */
    @Suppress("UNUSED_PARAMETER")
    fun quietAlarmNotifications(context: Context, ringing: List<Alert> = emptyList()) {
        AttentionNotifier.alarmDirect(true)
        AttentionNotifier.refresh(context)
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
    /** Returns true when the alert was unread until now (so an undo has something to restore). */
    fun markRead(context: Context, serverId: Long, alertId: String): Boolean {
        val store = Store.get(context)
        val changed = store.markRead(serverId, alertId)
        val a = store.alert(serverId, alertId) ?: return changed.also { Registry.changed() }
        if (a.action == "alarm" && a.stoppedAtMs == null) {
            stopAlarm(context, serverId, alertId)
        } else {
            AttentionNotifier.refresh(context)
            Registry.changed()
        }
        return changed
    }

    /** "Mark all as read". Returns the (server id, alert id) of the alerts it marked. */
    fun markAllRead(context: Context): List<Pair<Long, String>> {
        val marked = Store.get(context).unreadAlerts()
            .filter { markRead(context, it.serverId, it.alertId) }
            .map { it.serverId to it.alertId }
        AttentionNotifier.refresh(context)
        Registry.changed()
        return marked
    }

    /**
     * Undo of a mark-read: the alerts count as unread again. They do not alert again (the phone already showed them) and a
     * stopped alarm stays stopped.
     */
    fun markUnread(context: Context, alerts: List<Pair<Long, String>>) {
        val store = Store.get(context)
        for ((serverId, alertId) in alerts) store.markUnread(serverId, alertId)
        AttentionNotifier.refresh(context)
        Registry.changed()
    }

    /** Before a server is deleted: silences its alarms and removes it from the one notification. */
    fun forgetServer(context: Context, serverId: Long) {
        for (a in Store.get(context).serverAlerts(serverId)) {
            if (a.action == "alarm" && a.stoppedAtMs == null) AlarmPlayer.stop(context, serverId, a.alertId)
        }
        ServerClock.forget(context, serverId)
        ItemAlerter.forgetServer(context, serverId)
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
        AlarmPlayer.stop(context, serverId, alertId)
        AttentionNotifier.refresh(context)
        Registry.changed()
    }

    // ------------------------------------------------------------ ring

    internal fun playOnce(context: Context) {
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
    /** Ringer mode (silent / vibrate) the phone was in before the alarm forced it to normal. */
    private var savedRinger: Int? = null
    private var wakeLock: PowerManager.WakeLock? = null

    private fun key(serverId: Long, alertId: String) = "$serverId:$alertId"

    @Synchronized
    fun start(context: Context, a: Alert) {
        active.add(key(a.serverId, a.alertId))
        if (player != null) return
        val app = context.applicationContext
        val audio = app.getSystemService(AudioManager::class.java)
        // Leave silent / vibrate while it rings (when allowed to); stop() puts the mode back.
        val nm = app.getSystemService(NotificationManager::class.java)
        val ringer = audio.ringerMode
        if (ringer != AudioManager.RINGER_MODE_NORMAL && nm.isNotificationPolicyAccessGranted) {
            try {
                audio.ringerMode = AudioManager.RINGER_MODE_NORMAL
                if (savedRinger == null) savedRinger = ringer
            } catch (e: SecurityException) {
                Log.w(TAG, "cannot change ringer mode", e)
            }
        }
        try {
            if (savedVolume == null) savedVolume = audio.getStreamVolume(AudioManager.STREAM_ALARM)
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
        val audio = app.getSystemService(AudioManager::class.java)
        savedVolume?.let {
            try {
                audio.setStreamVolume(AudioManager.STREAM_ALARM, it, 0)
            } catch (_: SecurityException) {
            }
        }
        savedVolume = null
        // Back to silent / vibrate, unless the user changed the mode while it was ringing.
        savedRinger?.let {
            val nm = app.getSystemService(NotificationManager::class.java)
            if (audio.ringerMode == AudioManager.RINGER_MODE_NORMAL && nm.isNotificationPolicyAccessGranted) {
                try {
                    audio.ringerMode = it
                } catch (e: SecurityException) {
                    Log.w(TAG, "cannot restore ringer mode", e)
                }
            }
        }
        savedRinger = null
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
    }

    @Synchronized
    fun isRinging(): Boolean = active.isNotEmpty()
}

/** "Stop alarm" action of the one notification (local only). It stops every alarm that rings. A call with an alarm id stops that one. */
class StopReceiver : BroadcastReceiver() {
    companion object {
        const val EXTRA_SERVER = "server_id"
        const val EXTRA_ALERT = "alert_id"
    }

    override fun onReceive(context: Context, intent: Intent) {
        val serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val alertId = intent.getStringExtra(EXTRA_ALERT)
        if (serverId < 0 || alertId == null) {
            Alerter.stopAllAlarms(context)
            return
        }
        Alerter.stopAlarm(context, serverId, alertId)
    }
}
