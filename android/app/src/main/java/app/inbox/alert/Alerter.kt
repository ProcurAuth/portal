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
import android.util.Log
import app.inbox.R
import app.inbox.data.Alert
import app.inbox.data.Store
import app.inbox.service.Registry
import app.inbox.ui.AlarmActivity
import app.inbox.ui.ServerActivity

/**
 * Turns a stored alert into what the subscriber asked for:
 * - `alarm`: full-screen alert, looping alarm sound at full alarm volume through
 *   silent / vibrate / do-not-disturb, vibration, until a human acknowledges it;
 * - `ring`: one sound on the alarm stream (audible in silent / vibrate) + vibration;
 * - `notify`: a standard notification that follows the phone sound mode.
 */
object Alerter {
    const val CH_ALARM = "alarm"
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
        val alarm = NotificationChannel(CH_ALARM, "Alarmes", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "Alertes critiques : sonnent jusqu'à l'acquittement, même en silencieux"
            setSound(null, null) // played by AlarmPlayer on the alarm stream
            enableVibration(false) // vibration handled by AlarmPlayer
            setBypassDnd(true)
            lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        }
        val ring = NotificationChannel(CH_RING, "Sonneries", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "Un son, même en silencieux ou en vibreur"
            setSound(null, null)
            enableVibration(false)
            setBypassDnd(true)
            lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        }
        val notify = NotificationChannel(CH_NOTIFY, "Notifications", NotificationManager.IMPORTANCE_DEFAULT).apply {
            description = "Notifications standard, selon le mode du téléphone"
        }
        val status = NotificationChannel(CH_STATUS, "État de la connexion", NotificationManager.IMPORTANCE_MIN).apply {
            description = "Notification permanente du service Inbox"
            setShowBadge(false)
        }
        val warning = NotificationChannel(CH_WARNING, "Perte de connexion", NotificationManager.IMPORTANCE_HIGH).apply {
            description = "Prévient quand Inbox ne peut plus recevoir d'alertes"
        }
        nm.createNotificationChannels(listOf(alarm, ring, notify, status, warning))
    }

    /** Notification tag/id: unique per alert, so re-posting replaces instead of duplicating. */
    private fun tag(a: Alert) = "alert:${a.serverId}:${a.alertId}"
    private const val ALERT_NOTIFICATION_ID = 1000

    private fun requestCode(a: Alert, salt: Int) = (tag(a).hashCode() * 31) + salt

    fun present(context: Context, a: Alert) {
        val nm = context.getSystemService(NotificationManager::class.java)
        val title = a.title ?: a.groupName
        val body = a.message ?: "Alerte ${a.groupName}"
        val open = PendingIntent.getActivity(
            context, requestCode(a, 1),
            Intent(context, ServerActivity::class.java).putExtra(ServerActivity.EXTRA_SERVER, a.serverId),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val ack = PendingIntent.getBroadcast(
            context, requestCode(a, 2),
            Intent(context, AckReceiver::class.java)
                .putExtra(AckReceiver.EXTRA_SERVER, a.serverId)
                .putExtra(AckReceiver.EXTRA_ALERT, a.alertId),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val channel = when (a.action) {
            "alarm" -> CH_ALARM
            "ring" -> CH_RING
            else -> CH_NOTIFY
        }
        val b = Notification.Builder(context, channel)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(Notification.BigTextStyle().bigText(body))
            .setWhen(a.createdAtMs)
            .setShowWhen(true)
            .setContentIntent(open)
            .setAutoCancel(a.action != "alarm")
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .addAction(Notification.Action.Builder(null, "Acquitter", ack).build())
        if (a.action == "alarm") {
            val full = PendingIntent.getActivity(
                context, requestCode(a, 3), AlarmActivity.intent(context, a.serverId, a.alertId),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            b.setCategory(Notification.CATEGORY_ALARM)
                .setOngoing(true)
                .setFullScreenIntent(full, true)
                .setContentIntent(full)
                .setColor(0xFFB71C1C.toInt())
        } else {
            b.setCategory(Notification.CATEGORY_MESSAGE)
        }
        try {
            nm.notify(tag(a), ALERT_NOTIFICATION_ID, b.build())
        } catch (e: SecurityException) {
            Log.w(TAG, "notification permission missing", e)
        }
        when (a.action) {
            "alarm" -> AlarmPlayer.start(context, a)
            "ring" -> playOnce(context)
            else -> {}
        }
    }

    /** Called when the human acknowledges an alert, from any entry point. */
    fun acknowledge(context: Context, serverId: Long, alertId: String) {
        val store = Store.get(context)
        store.markAcked(serverId, alertId)
        store.enqueueAck(serverId, alertId, "acknowledged")
        val a = store.alert(serverId, alertId)
        if (a != null) {
            context.getSystemService(NotificationManager::class.java).cancel(tag(a), ALERT_NOTIFICATION_ID)
        }
        AlarmPlayer.stop(context, serverId, alertId)
        Registry.connections[serverId]?.flushOutbox()
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
 * The looping alarm. Keeps ringing while at least one alarm is unacknowledged.
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
        // "Sortir du vibreur": restore normal ringer mode when allowed to.
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

/** "Acquitter" action on alert notifications. */
class AckReceiver : BroadcastReceiver() {
    companion object {
        const val EXTRA_SERVER = "server_id"
        const val EXTRA_ALERT = "alert_id"
    }

    override fun onReceive(context: Context, intent: Intent) {
        val serverId = intent.getLongExtra(EXTRA_SERVER, -1)
        val alertId = intent.getStringExtra(EXTRA_ALERT) ?: return
        if (serverId < 0) return
        Alerter.acknowledge(context, serverId, alertId)
    }
}
