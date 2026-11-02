package app.portalinbox.service

import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import app.portalinbox.R
import app.portalinbox.alert.AlarmPlayer
import app.portalinbox.alert.Alerter
import app.portalinbox.data.Store
import app.portalinbox.ui.MainActivity
import app.portalinbox.ui.formatSince

/**
 * Foreground service holding one persistent session per enrolled server. It is
 * sticky, restarted at boot and by a watchdog alarm, so alerts keep arriving with
 * the app closed and no third-party push service.
 */
class InboxService : Service() {
    companion object {
        private const val TAG = "Inbox.Service"
        private const val STATUS_ID = 1
        const val WARNING_BASE_ID = 2000
        private const val TICK_MS = 15_000L
        private const val UNACKED_ALARM_RESUME_MS = 24 * 3600_000L
        private const val LOCAL_HISTORY_MS = 30L * 24 * 3600_000L

        fun start(context: Context) {
            try {
                context.startForegroundService(Intent(context, InboxService::class.java))
            } catch (e: Exception) {
                // Background start restrictions: the watchdog or the next app launch retries.
                Log.w(TAG, "cannot start service now", e)
            }
        }
    }

    private val handler = Handler(Looper.getMainLooper())
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    private val warned = mutableSetOf<Long>()
    private var recovered = false
    private val onChange: () -> Unit = { updateStatus() }

    private val tick = object : Runnable {
        override fun run() {
            updateStatus()
            handler.postDelayed(this, TICK_MS)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        val n = statusNotification(getString(R.string.svc_starting))
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(STATUS_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(STATUS_ID, n)
        }
        Registry.addListener(onChange)
        val cm = getSystemService(ConnectivityManager::class.java)
        networkCallback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                Registry.connections.values.forEach { it.wake() }
            }
        }.also { cm.registerDefaultNetworkCallback(it) }
        handler.post(tick)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (!recovered) {
            recovered = true
            recoverLocalState()
        }
        syncConnections()
        WatchdogReceiver.schedule(this)
        return START_STICKY
    }

    override fun onDestroy() {
        handler.removeCallbacksAndMessages(null)
        Registry.removeListener(onChange)
        networkCallback?.let { getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(it) }
        Registry.connections.values.forEach { it.stop() }
        Registry.connections.clear()
        super.onDestroy()
    }

    /** Alerts stored but never shown (process died in between) are shown now; alarms not stopped ring again. */
    private fun recoverLocalState() {
        val store = Store.get(this)
        for (a in store.unnotified()) {
            Alerter.present(this, a)
            store.markNotified(a.serverId, a.alertId)
        }
        val cutoff = System.currentTimeMillis() - UNACKED_ALARM_RESUME_MS
        for (a in store.ringingAlarms()) {
            if (a.receivedAtMs >= cutoff && !AlarmPlayer.isRinging()) Alerter.present(this, a)
        }
        store.prune(System.currentTimeMillis() - LOCAL_HISTORY_MS)
    }

    private fun syncConnections() {
        val servers = Store.get(this).servers()
        val ids = servers.map { it.id }.toSet()
        for ((id, c) in Registry.connections) {
            if (id !in ids) {
                c.stop()
                Registry.connections.remove(id)
                Registry.forget(id)
            }
        }
        for (s in servers) {
            Registry.connections.computeIfAbsent(s.id) { ServerConnection(applicationContext, s.id).also { it.start() } }
                .wake()
        }
        updateStatus()
    }

    private fun statusNotification(text: String): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE
        )
        return Notification.Builder(this, Alerter.CH_STATUS)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setStyle(Notification.BigTextStyle().bigText(text))
            .setOngoing(true)
            .setShowWhen(false)
            .setContentIntent(open)
            .build()
    }

    private fun updateStatus() {
        val store = Store.get(this)
        val servers = store.servers()
        val nm = getSystemService(NotificationManager::class.java)
        val now = System.currentTimeMillis()
        val lines = mutableListOf<String>() // servers that are not connected
        var connected = 0
        for (s in servers) {
            val st = Registry.status(s.id)
            val line = when (st.state) {
                LinkState.CONNECTED -> { connected++; null }
                LinkState.CONNECTING -> getString(R.string.svc_connecting, s.serverName)
                LinkState.DISCONNECTED -> getString(R.string.svc_disconnected, s.serverName, formatSince(now - st.sinceMs))
                LinkState.REENROLL -> getString(R.string.svc_reenroll, s.serverName)
            }
            if (line != null) lines.add(line)
            val offline = st.state != LinkState.CONNECTED &&
                (st.state == LinkState.REENROLL || now - st.sinceMs >= s.offlineWarningMs)
            val id = WARNING_BASE_ID + s.id.toInt()
            if (offline && s.id !in warned) {
                warned.add(s.id)
                val w = Notification.Builder(this, Alerter.CH_WARNING)
                    .setSmallIcon(R.drawable.ic_notification)
                    .setContentTitle(getString(R.string.svc_warning_title, s.serverName))
                    .setContentText(st.detail ?: getString(R.string.svc_warning_text))
                    .setStyle(Notification.BigTextStyle().bigText(getString(R.string.svc_warning_big, s.serverName, st.detail ?: "")))
                    .setContentIntent(PendingIntent.getActivity(this, id, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE))
                    .build()
                try {
                    nm.notify(id, w)
                } catch (e: SecurityException) {
                    Log.w(TAG, "notification permission missing", e)
                }
            } else if (!offline && s.id in warned) {
                warned.remove(s.id)
                nm.cancel(id)
            }
        }
        // "1/2 servers connected", then one line per server that is not.
        val summary = if (servers.isEmpty()) {
            getString(R.string.svc_no_server)
        } else {
            val count = resources.getQuantityString(R.plurals.servers_connected, servers.size, connected, servers.size)
            (listOf(count) + lines).joinToString("\n")
        }
        try {
            nm.notify(STATUS_ID, statusNotification(summary))
        } catch (e: SecurityException) {
            Log.w(TAG, "notification permission missing", e)
        }
    }
}

/** Starts the service after boot and after an app update. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED -> InboxService.start(context)
        }
    }
}

/** Inexact periodic alarm restarting the service if the system killed it. */
class WatchdogReceiver : BroadcastReceiver() {
    companion object {
        private const val INTERVAL_MS = 15 * 60_000L

        fun schedule(context: Context) {
            val am = context.getSystemService(AlarmManager::class.java)
            val pi = PendingIntent.getBroadcast(
                context, 0, Intent(context, WatchdogReceiver::class.java),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            am.setAndAllowWhileIdle(AlarmManager.ELAPSED_REALTIME_WAKEUP, SystemClock.elapsedRealtime() + INTERVAL_MS, pi)
        }
    }

    override fun onReceive(context: Context, intent: Intent) {
        InboxService.start(context)
        schedule(context)
    }
}
