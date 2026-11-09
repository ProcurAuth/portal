package app.portalinbox.ui

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.os.Handler
import android.os.Looper
import android.util.Log

/**
 * Whether the phone has a network, for the banner "No network" of the home list (proposal 3.11). It follows the default
 * network while a screen is on. A network without internet still counts: a portal can sit on the local network.
 * [onChange] runs on the main thread.
 */
class NetworkState(private val context: Context, private val onChange: () -> Unit) {
    @Volatile
    var available: Boolean = true
        private set

    private val main = Handler(Looper.getMainLooper())
    private var callback: ConnectivityManager.NetworkCallback? = null

    fun start() {
        val cm = context.getSystemService(ConnectivityManager::class.java) ?: return
        available = cm.activeNetwork != null
        if (callback != null) return
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = set(true)
            override fun onLost(network: Network) = set(cm.activeNetwork != null)
        }
        try {
            cm.registerDefaultNetworkCallback(cb)
            callback = cb
        } catch (e: SecurityException) {
            // Without the permission the banner stays off: the portals show their own state.
            Log.w("NetworkState", "network state not readable", e)
        }
    }

    fun stop() {
        val cb = callback ?: return
        callback = null
        try {
            context.getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(cb)
        } catch (e: IllegalArgumentException) {
            // Already unregistered.
        }
    }

    private fun set(value: Boolean) {
        main.post {
            if (available != value) {
                available = value
                onChange()
            }
        }
    }
}
